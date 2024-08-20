use std::collections::HashMap;
use crate::hooker::KatanaHooker;
use anyhow::Result;
use async_trait::async_trait;
use katana_primitives::chain::ChainId;
use katana_primitives::receipt::MessageToL1;
use katana_primitives::transaction::L1HandlerTx;
use katana_primitives::utils::transaction::compute_l1_message_hash;
use starknet::accounts::{Account, Call, ExecutionEncoding, SingleOwnerAccount};
use starknet::core::types::{BlockId, BlockTag, EmittedEvent, EventFilter, FieldElement};
use starknet::core::utils::starknet_keccak;
use starknet::macros::{felt, selector};
use starknet::providers::jsonrpc::HttpTransport;
use starknet::providers::{AnyProvider, JsonRpcClient, Provider};
use starknet::signers::{LocalWallet, SigningKey};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock as AsyncRwLock;
use tracing::{debug, error, info, trace, warn};
use url::Url;

use super::{Error, MessagingConfig, Messenger, MessengerResult, LOG_TARGET};

const MSG_MAGIC: FieldElement = felt!("0x4d5347");
const EXE_MAGIC: FieldElement = felt!("0x455845");

pub const HASH_EXEC: FieldElement = felt!("0xee");

pub struct StarknetMessaging<EF: katana_executor::ExecutorFactory + Send + Sync> {
    chain_id: FieldElement,
    provider: AnyProvider,
    wallet: LocalWallet,
    sender_account_address: FieldElement,
    messaging_contract_address: FieldElement,
    hooker: Arc<AsyncRwLock<dyn KatanaHooker<EF> + Send + Sync>>,
    event_cache: Arc<AsyncRwLock<HashSet<String>>>,
    latest_block: Arc<AtomicU64>,
}

impl<EF: katana_executor::ExecutorFactory + Send + Sync> StarknetMessaging<EF> {
    pub async fn new(
        config: MessagingConfig,
        hooker: Arc<AsyncRwLock<dyn KatanaHooker<EF> + Send + Sync>>,
    ) -> Result<StarknetMessaging<EF>> {
        let provider = AnyProvider::JsonRpcHttp(JsonRpcClient::new(HttpTransport::new(
            Url::parse(&config.rpc_url)?,
        )));

        let private_key = FieldElement::from_hex_be(&config.private_key)?;
        let key = SigningKey::from_secret_scalar(private_key);
        let wallet = LocalWallet::from_signing_key(key);
        let latest_block = Arc::new(AtomicU64::new(0));

        let chain_id = provider.chain_id().await?;
        let sender_account_address = FieldElement::from_hex_be(&config.sender_address)?;
        let messaging_contract_address = FieldElement::from_hex_be(&config.contract_address)?;

        info!(target: LOG_TARGET, "StarknetMessaging instance created.");

        Ok(StarknetMessaging {
            wallet,
            provider,
            chain_id,
            sender_account_address,
            messaging_contract_address,
            hooker,
            event_cache: Arc::new(AsyncRwLock::new(HashSet::new())),
            latest_block,
        })
    }

    /// Fetches events for the given blocks range.
    pub async fn fetch_events(
        &self,
        from_block: BlockId,
        to_block: BlockId,
    ) -> Result<HashMap<u64, Vec<EmittedEvent>>> {
        trace!(target: LOG_TARGET, from_block = ?from_block, to_block = ?to_block, "Fetching logs.");

        let mut block_to_events: HashMap<u64, Vec<EmittedEvent>> = HashMap::new();

        let filter = EventFilter {
            from_block: Some(from_block),
            to_block: Some(to_block),
            address: Some(self.messaging_contract_address),
            // TODO: this might come from the configuration actually.
            keys: None,
        };

        // TODO: this chunk_size may also come from configuration?
        let chunk_size = 200;
        let mut continuation_token: Option<String> = None;

        loop {
            let event_page =
                self.provider.get_events(filter.clone(), continuation_token, chunk_size).await?;

            event_page.events.into_iter().for_each(|event| {
                // We ignore events without the block number
                if let Some(block_number) = event.block_number {
                    block_to_events
                        .entry(block_number)
                        .and_modify(|v| v.push(event.clone()))
                        .or_insert(vec![event]);
                }
            });

            continuation_token = event_page.continuation_token;

            if continuation_token.is_none() {
                break;
            }
        }

        Ok(block_to_events)
    }

    async fn fetch_pending_events(&self, chain_id: ChainId) -> MessengerResult<Vec<L1HandlerTx>> {
        let mut l1_handler_txs: Vec<L1HandlerTx> = vec![];
        let mut continuation_token: Option<String> = None;

        loop {
            debug!(target: LOG_TARGET, "Fetching pending events with continuation token: {:?}", continuation_token);

            let filter = EventFilter {
                from_block: Some(BlockId::Tag(BlockTag::Pending)),
                to_block: Some(BlockId::Tag(BlockTag::Pending)),
                address: Some(self.messaging_contract_address),
                keys: None,
            };

            let event_page = self
                .provider
                .get_events(filter.clone(), continuation_token.clone(), 200)
                .await
                .map_err(|e| {
                    error!(target: LOG_TARGET, "Error fetching pending events: {:?}", e);
                    Error::SendError
                })?;

            debug!(target: LOG_TARGET, "Fetched {} events", event_page.events.len());

            for event in event_page.events {
                let event_id = event.transaction_hash.to_string();
                debug!(target: LOG_TARGET, "Processing event with ID: {}", event_id);

                {
                    let cache = self.event_cache.read().await;
                    if cache.contains(&event_id) {
                        debug!(target: LOG_TARGET, "Event ID: {} already processed, skipping", event_id);
                        continue;
                    }
                }

                if let Ok(tx) = l1_handler_tx_from_event(&event, chain_id) {
                    if let Ok((from, to, selector)) = info_from_event(&event) {
                     let hooker = Arc::clone(&self.hooker);
                         let is_message_accepted = hooker
                             .read()
                             .await
                             .verify_message_to_appchain(from, to, selector)
                             .await;
                        if is_message_accepted {
                            debug!(target: LOG_TARGET, "Event ID: {} accepted, adding to transactions", event_id);
                            l1_handler_txs.push(tx);
                            let mut cache = self.event_cache.write().await;
                            cache.insert(event_id);
                        } else {
                             debug!(
                                 target: LOG_TARGET,
                                 "Event ID: {} not accepted by hooker, check the contract addresses defined in the hooker: executor address: {:?}, orderbook address: {:?}",
                                 event_id,
                                 from,
                                 to
                             );
                        }
                    }
                }
            }

            continuation_token = event_page.continuation_token;

            if continuation_token.is_none() {
                break;
            }
        }

        debug!(target: LOG_TARGET, "Total transactions gathered: {}", l1_handler_txs.len());
        Ok(l1_handler_txs)
    }

    async fn send_invoke_tx(&self, calls: Vec<Call>) -> Result<FieldElement> {
        let signer = Arc::new(&self.wallet);
        let mut account = SingleOwnerAccount::new(
            &self.provider,
            signer,
            self.sender_account_address,
            self.chain_id,
            ExecutionEncoding::New,
        );

        info!(target: LOG_TARGET, "Setting block ID to Pending.");
        account.set_block_id(BlockId::Tag(BlockTag::Pending));

        let execution = account.execute(calls).fee_estimate_multiplier(10f64);
        let estimated_fee = match execution.estimate_fee().await {
            Ok(fee) => {
                info!(target: LOG_TARGET, "Estimated fee: {:?}", fee.overall_fee);
                (fee.overall_fee) * 10u64.into()
            }
            Err(e) => {
                error!(target: LOG_TARGET, "Error estimating fee: {:?}", e);
                return Err(e.into());
            }
        };

        let execution_with_fee = execution.max_fee(estimated_fee);
        info!(target: LOG_TARGET, "Sending invoke transaction with max fee: {:?}", estimated_fee);

        match execution_with_fee.send().await {
            Ok(tx) => {
                info!(target: LOG_TARGET, "Transaction successful: {:?}", tx);
                Ok(tx.transaction_hash)
            }
            Err(e) => {
                error!(target: LOG_TARGET, "Error sending transaction: {:?}", e);
                Err(e.into())
            }
        }
    }

    async fn send_hashes(&self, mut hashes: Vec<FieldElement>) -> MessengerResult<FieldElement> {
        hashes.retain(|&x| x != HASH_EXEC);

        if hashes.is_empty() {
            info!(target: LOG_TARGET, "No hashes to send.");
            return Ok(FieldElement::ZERO);
        }

        info!(target: LOG_TARGET, "Preparing to send {} hashes.", hashes.len());

        let mut calldata = hashes.clone();
        calldata.insert(0, calldata.len().into());

        let call = Call {
            selector: selector!("add_messages_hashes_from_appchain"),
            to: self.messaging_contract_address,
            calldata: calldata.clone(),
        };

        info!(target: LOG_TARGET, "Sending hashes to Starknet: {:?}", calldata);

        match self.send_invoke_tx(vec![call]).await {
            Ok(tx_hash) => {
                trace!(target: LOG_TARGET, tx_hash = %format!("{:#064x}", tx_hash), "Hashes sending transaction.");
                info!(target: LOG_TARGET, "Successfully sent hashes with transaction hash: {:#064x}", tx_hash);
                Ok(tx_hash)
            }
            Err(e) => {
                error!(target: LOG_TARGET, error = %e, "Settling hashes on Starknet.");
                Err(Error::SendError)
            }
        }
    }
}

#[async_trait]
impl<EF: katana_executor::ExecutorFactory + Send + Sync> Messenger for StarknetMessaging<EF> {
    type MessageHash = FieldElement;
    type MessageTransaction = L1HandlerTx;

    async fn gather_messages(
        &self,
        from_block: u64,
        max_blocks: u64,
        chain_id: ChainId,
    ) -> MessengerResult<(u64, Vec<L1HandlerTx>)> {
        debug!(target: LOG_TARGET, "Gathering messages");

        let chain_latest_block: u64 = match self.provider.block_number().await {
            Ok(n) => {
                debug!(target: LOG_TARGET, "Latest block number on chain: {}", n);
                n
            }
            Err(e) => {
                warn!(
                    target: LOG_TARGET,
                    "Couldn't fetch settlement chain last block number. Skipped, retry at the next tick. Error: {:?}", e
                );
                return Err(Error::SendError);
            }
        };

        if from_block > chain_latest_block {
            // Nothing to fetch, we can skip waiting the next tick.
            return Ok((chain_latest_block, vec![]));
        }

        // +1 as the from_block counts as 1 block fetched.
        let to_block = if from_block + max_blocks + 1 < chain_latest_block {
            from_block + max_blocks
        } else {
            chain_latest_block
        };

        let mut l1_handler_txs: Vec<L1HandlerTx> = vec![];

        // fetch events for the given range before fetching pending events
        self.fetch_events(BlockId::Number(from_block), BlockId::Number(to_block))
            .await
            .map_err(|_| Error::SendError)
            .unwrap()
            .iter()
            .for_each(|(block_number, block_events)| {
                debug!(
                    target: LOG_TARGET,
                    block_number = %block_number,
                    events_count = %block_events.len(),
                    "Converting events of block into L1HandlerTx."
                );

                block_events.iter().for_each(|e| {
                    if let Ok(tx) = l1_handler_tx_from_event(e, chain_id) {
                        l1_handler_txs.push(tx)
                    }
                })
            });

        // Check if the block number has changed
        let previous_block = self.latest_block.load(Ordering::Relaxed);
        if previous_block != chain_latest_block {
            debug!(target: LOG_TARGET, "Block number changed from {} to {}, clearing cache", previous_block, chain_latest_block);
            self.event_cache.write().await.clear();
            self.latest_block.store(chain_latest_block, Ordering::Relaxed);
        }
        // Fetch pending events
        let pending_txs = self.fetch_pending_events(chain_id).await?;
        // Add pending events to the list
        l1_handler_txs.extend(pending_txs);

        debug!(target: LOG_TARGET, "Returning {} transactions", l1_handler_txs.len());
        Ok((chain_latest_block, l1_handler_txs))
    }

    async fn send_messages(
        &self,
        messages: &[MessageToL1],
    ) -> MessengerResult<Vec<Self::MessageHash>> {
        if messages.is_empty() {
            info!(target: LOG_TARGET, "No messages to send.");
            return Ok(vec![]);
        }

        let (hashes, calls) = parse_messages(messages)?;
        for call in &calls {
            if !self.hooker.read().await.verify_tx_for_starknet(call.clone()).await {
                warn!(target: LOG_TARGET, "Call verification failed for call: {:?}", call);
                continue;
            }
        }

        if !calls.is_empty() {
            info!(target: LOG_TARGET, "Sending {} calls.", calls.len());
            if let Err(e) = self.send_invoke_tx(calls.clone()).await {
                error!(target: LOG_TARGET, error = %e, "Error sending invoke transaction.");
                for call in calls {
                    self.hooker.read().await.on_starknet_tx_failed(call).await;
                }
                return Err(Error::SendError);
            }
            info!(target: LOG_TARGET, "Successfully sent invoke transaction.");
        }

        if let Err(e) = self.send_hashes(hashes.clone()).await {
            error!(target: LOG_TARGET, error = %e, "Error sending hashes.");
            return Err(Error::SendError);
        }
        info!(target: LOG_TARGET, "Successfully sent hashes.");

        info!(target: LOG_TARGET, "Finished sending messages.");
        Ok(hashes)
    }
}

fn parse_messages(messages: &[MessageToL1]) -> MessengerResult<(Vec<FieldElement>, Vec<Call>)> {
    let mut hashes: Vec<FieldElement> = vec![];
    let mut calls: Vec<Call> = vec![];

    for m in messages {
        let magic = m.to_address;

        if magic == EXE_MAGIC {
            if m.payload.len() < 2 {
                error!(
                    target: LOG_TARGET,
                    "Message execution is expecting a payload of at least length \
                     2. With [0] being the contract address, and [1] the selector.",
                );
                continue;
            }

            let to = m.payload[0];
            let selector = m.payload[1];

            let mut calldata = vec![];
            if m.payload.len() >= 3 {
                calldata.extend(m.payload[2..].to_vec());
            }

            calls.push(Call { to, selector, calldata });
            hashes.push(HASH_EXEC);
        } else if magic == MSG_MAGIC {
            let to_address = m.payload[0];
            let payload = &m.payload[1..];

            let mut buf: Vec<u8> = vec![];
            buf.extend(m.from_address.to_bytes_be());
            buf.extend(to_address.to_bytes_be());
            buf.extend(FieldElement::from(payload.len()).to_bytes_be());
            for p in payload {
                buf.extend(p.to_bytes_be());
            }

            hashes.push(starknet_keccak(&buf));
        } else {
            warn!(target: LOG_TARGET, magic = ?magic, "Invalid message to_address magic value.");
            continue;
        }
    }

    Ok((hashes, calls))
}

fn l1_handler_tx_from_event(event: &EmittedEvent, chain_id: ChainId) -> Result<L1HandlerTx> {
    if event.keys[0] != selector!("MessageSentToAppchain") {
        debug!(
            target: LOG_TARGET,
            event_key = ?event.keys[0],
            "Event can't be converted into L1HandlerTx."
        );
        return Err(Error::GatherError.into());
    }

    if event.keys.len() != 4 || event.data.len() < 2 {
        error!(target: LOG_TARGET, "Event MessageSentToAppchain is not well formatted.");
    }

    let from_address = event.keys[2];
    let to_address = event.keys[3];
    let entry_point_selector = event.data[0];
    let nonce = event.data[1];

    let mut calldata = vec![from_address];
    calldata.extend(&event.data[3..]);

    let message_hash = compute_l1_message_hash(from_address, to_address, &calldata);

    Ok(L1HandlerTx {
        nonce,
        calldata,
        chain_id,
        message_hash,
        paid_fee_on_l1: 30000_u128,
        entry_point_selector,
        version: FieldElement::ZERO,
        contract_address: to_address.into(),
    })
}

fn info_from_event(event: &EmittedEvent) -> Result<(FieldElement, FieldElement, FieldElement)> {
    if event.keys[0] != selector!("MessageSentToAppchain") {
        debug!(
            target: LOG_TARGET,
            "Event with key {:?} can't be converted into L1HandlerTx", event.keys[0],
        );
        return Err(Error::GatherError.into());
    }

    if event.keys.len() != 4 || event.data.len() < 2 {
        error!(target: LOG_TARGET, "Event MessageSentToAppchain is not well formatted");
    }

    let from_address = event.keys[2];
    let to_address = event.keys[3];
    let entry_point_selector = event.data[0];

    Ok((from_address, to_address, entry_point_selector))
}

#[cfg(test)]
mod tests {
    use katana_primitives::utils::transaction::compute_l1_handler_tx_hash;
    use starknet::macros::felt;

    use super::*;

    #[test]
    fn parse_messages_msg() {
        let from_address = selector!("from_address");
        let to_address = selector!("to_address");
        let selector = selector!("selector");
        let payload_msg = vec![to_address, FieldElement::ONE, FieldElement::TWO];
        let payload_exe = vec![to_address, selector, FieldElement::ONE, FieldElement::TWO];

        let messages = vec![
            MessageToL1 {
                from_address: from_address.into(),
                to_address: MSG_MAGIC,
                payload: payload_msg,
            },
            MessageToL1 {
                from_address: from_address.into(),
                to_address: EXE_MAGIC,
                payload: payload_exe.clone(),
            },
        ];

        let (hashes, calls) = parse_messages(&messages).unwrap();

        assert_eq!(hashes.len(), 2);
        assert_eq!(
            hashes,
            vec![
                FieldElement::from_hex_be(
                    "0x03a1d2e131360f15e26dd4f6ff10550685611cc25f75e7950b704adb04b36162"
                )
                .unwrap(),
                HASH_EXEC,
            ]
        );

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].to, to_address);
        assert_eq!(calls[0].selector, selector);
        assert_eq!(calls[0].calldata, payload_exe[2..].to_vec());
    }

    #[test]
    #[should_panic]
    fn parse_messages_msg_bad_payload() {
        let from_address = selector!("from_address");
        let payload_msg = vec![];

        let messages = vec![MessageToL1 {
            from_address: from_address.into(),
            to_address: MSG_MAGIC,
            payload: payload_msg,
        }];

        parse_messages(&messages).unwrap();
    }

    #[test]
    #[should_panic]
    fn parse_messages_exe_bad_payload() {
        let from_address = selector!("from_address");
        let payload_exe = vec![FieldElement::ONE];

        let messages = vec![MessageToL1 {
            from_address: from_address.into(),
            to_address: EXE_MAGIC,
            payload: payload_exe,
        }];

        parse_messages(&messages).unwrap();
    }

    #[test]
    fn l1_handler_tx_from_event_parse_ok() {
        let from_address = selector!("from_address");
        let to_address = selector!("to_address");
        let selector = selector!("selector");
        let chain_id = ChainId::parse("KATANA").unwrap();
        let nonce = FieldElement::ONE;
        let calldata = vec![from_address, FieldElement::THREE];

        let transaction_hash: FieldElement = compute_l1_handler_tx_hash(
            FieldElement::ZERO,
            to_address,
            selector,
            &calldata,
            chain_id.into(),
            nonce,
        );

        let event = EmittedEvent {
            from_address: felt!(
                "0x049d36570d4e46f48e99674bd3fcc84644ddd6b96f7c741b1562b82f9e004dc7"
            ),
            keys: vec![
                selector!("MessageSentToAppchain"),
                selector!("random_hash"),
                from_address,
                to_address,
            ],
            data: vec![
                selector,
                nonce,
                FieldElement::from(calldata.len() as u128),
                FieldElement::THREE,
            ],
            block_hash: Some(selector!("block_hash")),
            block_number: Some(0),
            transaction_hash,
        };

        let message_hash = compute_l1_message_hash(from_address, to_address, &calldata);

        let expected = L1HandlerTx {
            nonce,
            calldata,
            chain_id,
            message_hash,
            paid_fee_on_l1: 30000_u128,
            version: FieldElement::ZERO,
            entry_point_selector: selector,
            contract_address: to_address.into(),
        };

        let tx = l1_handler_tx_from_event(&event, chain_id).unwrap();

        assert_eq!(tx, expected);
    }

    #[test]
    #[should_panic]
    fn l1_handler_tx_from_event_parse_bad_selector() {
        let from_address = selector!("from_address");
        let to_address = selector!("to_address");
        let selector = selector!("selector");
        let nonce = FieldElement::ONE;
        let calldata = [from_address, FieldElement::THREE];
        let transaction_hash = FieldElement::ZERO;

        let event = EmittedEvent {
            from_address: felt!(
                "0x049d36570d4e46f48e99674bd3fcc84644ddd6b96f7c741b1562b82f9e004dc7"
            ),
            keys: vec![
                selector!("AnOtherUnexpectedEvent"),
                selector!("random_hash"),
                from_address,
                to_address,
            ],
            data: vec![
                selector,
                nonce,
                FieldElement::from(calldata.len() as u128),
                FieldElement::THREE,
            ],
            block_hash: Some(selector!("block_hash")),
            block_number: Some(0),
            transaction_hash,
        };

        let _tx = l1_handler_tx_from_event(&event, ChainId::default()).unwrap();
    }

    #[test]
    #[should_panic]
    fn l1_handler_tx_from_event_parse_missing_key_data() {
        let from_address = selector!("from_address");
        let _to_address = selector!("to_address");
        let _selector = selector!("selector");
        let _nonce = FieldElement::ONE;
        let _calldata = [from_address, FieldElement::THREE];
        let transaction_hash = FieldElement::ZERO;

        let event = EmittedEvent {
            from_address: felt!(
                "0x049d36570d4e46f48e99674bd3fcc84644ddd6b96f7c741b1562b82f9e004dc7"
            ),
            keys: vec![selector!("AnOtherUnexpectedEvent"), selector!("random_hash"), from_address],
            data: vec![],
            block_hash: Some(selector!("block_hash")),
            block_number: Some(0),
            transaction_hash,
        };

        let _tx = l1_handler_tx_from_event(&event, ChainId::default()).unwrap();
    }
}
