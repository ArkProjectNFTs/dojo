use crate::service::messaging::{utils::{update_l3, update_l2_hash, update_l2_block}, starknet::StarknetMessaging};
use alloy_primitives::B256;

use crate::hooker::KatanaHooker;
use std::sync::atomic::Ordering;
use tokio::sync::RwLock as AsyncRwLock;

use futures::{Future, FutureExt, Stream};
use katana_executor::ExecutorFactory;
use katana_primitives::block::BlockHashOrNumber;
use katana_primitives::receipt::MessageToL1;
use katana_primitives::transaction::{ExecutableTxWithHash, L1HandlerTx, TxHash};
use katana_provider::traits::block::BlockNumberProvider;
use katana_provider::traits::transaction::ReceiptProvider;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::{interval_at, Instant, Interval};
use tracing::{error, info};

use super::{MessagingConfig, Messenger, MessengerMode, MessengerResult, LOG_TARGET};
use crate::backend::Backend;
use crate::pool::TransactionPool;

type MessagingFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type MessageGatheringFuture = MessagingFuture<MessengerResult<(u64, usize)>>;
type MessageSettlingFuture = MessagingFuture<MessengerResult<Option<(u64, usize)>>>;

pub struct MessagingService<EF: ExecutorFactory> {
    /// The interval at which the service will perform the messaging operations.
    interval: Interval,
    backend: Arc<Backend<EF>>,
    pool: Arc<TransactionPool>,
    /// The messenger mode the service is running in.
    messenger: Arc<MessengerMode<EF>>,
    /// The block number of the settlement chain from which messages will be gathered.
    gather_from_block: u64,
    /// The message gathering future.
    msg_gather_fut: Option<MessageGatheringFuture>,
    /// The block number of the local blockchain from which messages will be sent.
    send_from_block: u64,
    /// The message sending future.
    msg_send_fut: Option<MessageSettlingFuture>,

    tx_hash: B256,

    path : String,
}

impl<EF: ExecutorFactory> MessagingService<EF> {
    /// Initializes a new instance from a configuration file's path.
    /// Will panic on failure to avoid continuing with invalid configuration.
    pub async fn new(
        config: MessagingConfig,
        pool: Arc<TransactionPool>,
        backend: Arc<Backend<EF>>,
        hooker: Arc<AsyncRwLock<dyn KatanaHooker<EF> + Send + Sync>>,
    ) -> anyhow::Result<Self> {
        let gather_from_block: u64 = config.gather_from_block;
        let send_from_block: u64 = config.send_from_block;
        let tx_hash: B256 = config.tx_hash;
        let interval = interval_from_seconds(config.interval);
        let path = config.clone().path;
        let messenger = match MessengerMode::from_config(config, hooker).await {
            Ok(m) => Arc::new(m),
            Err(_) => {
                panic!(
                    "Messaging could not be initialized.\nVerify that the messaging target node \
                     (anvil or other katana) is running.\n"
                )
            }
        };

        let _latest_block = match &*messenger {
            MessengerMode::Starknet(StarknetMessaging {
                wallet : _,
                provider : _,
                chain_id : _,
                sender_account_address : _,
                messaging_contract_address : _,
                hooker : _,
                event_cache : _,
                latest_block,
                path : _, 
                tx_hash: _,
            }) => {
                // Access and use the fields as needed
                latest_block.load(Ordering::SeqCst)  //à quoi ça sert??
            }
            _ => {
                0
            }
        };


        Ok(Self {
            pool,
            backend,
            interval,
            messenger,
            gather_from_block,
            //send_from_block: 0, //ici?
            send_from_block,
            msg_gather_fut: None,
            msg_send_fut: None,
            tx_hash,
            path: path
        })
    }

    async fn gather_messages(
        messenger: Arc<MessengerMode<EF>>,
        pool: Arc<TransactionPool>,
        backend: Arc<Backend<EF>>,
        gather_from_block: u64,
        tx_hash: B256,
        path: String,
    ) -> MessengerResult<(u64, usize)> {
        // 200 avoids any possible rejection from RPC with possibly lots of messages.
        // TODO: May this be configurable?
        let max_block = 200;

        match messenger.as_ref() {
            MessengerMode::Ethereum(inner) => {
                let (block_num, txs) =
                    inner.gather_messages(gather_from_block, max_block, backend.chain_id).await?;
                let txs_count = txs.len();

                txs.into_iter().for_each(|tx| {
                    let hash = tx.calculate_hash();
                    trace_l1_handler_tx_exec(hash, &tx);
                    pool.add_transaction(ExecutableTxWithHash { hash, transaction: tx.into() })
                });

                Ok((block_num, txs_count))
            }

            MessengerMode::Starknet(inner) => {
                let (block_num, txs) =
                    inner.gather_messages(gather_from_block, max_block, backend.chain_id).await?;
                let txs_count = txs.len();
                info!(target: LOG_TARGET, "Gathered {} transactions for Starknet mode.", txs_count);
                let mut count = 0;
                txs.into_iter().for_each(|tx: L1HandlerTx| {
                    let hash = tx.calculate_hash();
                    info!(target: LOG_TARGET, "Processing transaction with hash: {:#x}", hash);
                    trace_l1_handler_tx_exec(hash, &tx);
                    if count == 1 {
                        pool.add_transaction(ExecutableTxWithHash { hash, transaction: tx.clone().into() }); // ici condition, on skip et on ajoute le prochain
                    }
                    //ici, enregistrer msg hash et block number 
                    //tx.msg_hash et block_num permet de recup ce qu'on veut 
                    if tx.message_hash == tx_hash {
                        count = 1
                    }
                    update_l2_hash( tx.message_hash, path.clone()); //ça peut fail mais on a pas mieux . ne pas update le block ici, update que le hash
                });
                //update le block ici !, latest
                update_l2_block(block_num, path.clone());
                Ok((block_num, txs_count))
            }
        }
    }

    async fn send_messages(
        block_num: u64,
        backend: Arc<Backend<EF>>,
        messenger: Arc<MessengerMode<EF>>,
    ) -> MessengerResult<Option<(u64, usize)>> {
        // Retrieve messages to be sent from the local blockchain for the given block number
        let Some(messages) = ReceiptProvider::receipts_by_block(
            backend.blockchain.provider(),
            BlockHashOrNumber::Num(block_num),
        )
        .unwrap()
        .map(|r| r.iter().flat_map(|r| r.messages_sent().to_vec()).collect::<Vec<MessageToL1>>()) else {
            return Ok(None);
        };

        if messages.is_empty() {
            info!(target: LOG_TARGET, "No messages to send from block {}", block_num);
            return Ok(Some((block_num, 0)));
        }

        info!(target: LOG_TARGET, "Retrieved {} messages from block {}", messages.len(), block_num);

        //let block_num = ... on update ici le block number, et à l'interieur le hash (finalement pas besoin d'update le hash)
        //let (block_number, length)
        match messenger.as_ref() {
            MessengerMode::Ethereum(inner) => {
                match inner.send_messages(&messages).await {
                    Ok(hashes) => {
                        let hash_strings: Vec<String> =
                            hashes.iter().map(|h| format!("{:#x}", h)).collect();
                        trace_msg_to_l1_sent(&messages, &hash_strings);
                        info!(target: LOG_TARGET, "Successfully sent {} messages from block {}", hash_strings.len(), block_num);
                        Ok(Some((block_num, hash_strings.len())))
                    }
                    Err(e) => {
                        error!(target: LOG_TARGET, error = %e, "Error sending messages from block {}", block_num);
                        // Even if there's an error, we should move to the next block to avoid infinite retries
                        Ok(Some((block_num, 0))) // Marking as processed to avoid retries
                    }
                }
            }
            MessengerMode::Starknet(inner) => {
                match inner.send_messages(&messages).await {
                    Ok(hashes) => {
                        let hash_strings: Vec<String> =
                            hashes.iter().map(|h| format!("{:#x}", h)).collect();
                        trace_msg_to_l1_sent(&messages, &hash_strings);
                        info!(target: LOG_TARGET, "Successfully sent {} messages from block {}", hash_strings.len(), block_num);
                        update_l3(block_num, inner.path.clone());   //update here all the block
                        Ok(Some((block_num, hash_strings.len())))
                    }
                    Err(e) => {
                        error!(target: LOG_TARGET, error = %e, "Error sending messages from block {}", block_num);
                        // Even if there's an error, we should move to the next block to avoid infinite retries
                        Ok(Some((block_num, 0))) // Marking as processed to avoid retries
                    }
                }
               
            }
        }
    }
}

pub enum MessagingOutcome {
    Gather {
        /// The latest block number of the settlement chain from which messages were gathered.
        lastest_block: u64,
        /// The number of settlement chain messages gathered up until `latest_block`.
        msg_count: usize,
    },
    Send {
        /// The current local block number from which messages were sent.
        block_num: u64,
        /// The number of messages sent on `block_num`.
        msg_count: usize,
    },
}

impl<EF: ExecutorFactory> Stream for MessagingService<EF> {
    type Item = MessagingOutcome;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.as_mut().get_mut();
        let path: String = pin.path.clone(); // Clone the path before mutable borrow

        if pin.interval.poll_tick(cx).is_ready() {
            if pin.msg_gather_fut.is_none() {
                pin.msg_gather_fut = Some(Box::pin(Self::gather_messages(
                    pin.messenger.clone(),
                    pin.pool.clone(),
                    pin.backend.clone(),
                    pin.gather_from_block,
                    pin.tx_hash,
                    path,
                )));
            }

            if pin.msg_send_fut.is_none() {
                let local_latest_block_num =
                    BlockNumberProvider::latest_number(pin.backend.blockchain.provider()).unwrap();
                if pin.send_from_block <= local_latest_block_num {
                    pin.msg_send_fut = Some(Box::pin(Self::send_messages(
                        pin.send_from_block,
                        pin.backend.clone(),
                        pin.messenger.clone(),
                    )));
                }
            }
        }

        // Poll the gathering future
        if let Some(mut gather_fut) = pin.msg_gather_fut.take() {
            match gather_fut.poll_unpin(cx) {
                Poll::Ready(Ok((last_block, msg_count))) => {
                    info!(target: LOG_TARGET, "Gathered {} transactions up to block {}", msg_count, last_block);
                    pin.gather_from_block = last_block + 1;
                    return Poll::Ready(Some(MessagingOutcome::Gather {
                        lastest_block: last_block,
                        msg_count,
                    }));
                }
                Poll::Ready(Err(e)) => {
                    error!(target: LOG_TARGET, block = %pin.gather_from_block, error = %e, "Error gathering messages for block.");
                    return Poll::Pending;
                }
                Poll::Pending => pin.msg_gather_fut = Some(gather_fut),
            }
        }

        // Poll the message sending future
        if let Some(mut send_fut) = pin.msg_send_fut.take() {
            match send_fut.poll_unpin(cx) {
                Poll::Ready(Ok(Some((block_num, msg_count)))) => {
                    info!(target: LOG_TARGET, "Sent {} messages from block {}", msg_count, block_num);
                    pin.send_from_block = block_num + 1;
                    return Poll::Ready(Some(MessagingOutcome::Send { block_num, msg_count }));
                }
                Poll::Ready(Err(e)) => {
                    error!(target: LOG_TARGET, block = %pin.send_from_block, error = %e, "Error sending messages for block.");
                    // Even if there's an error, we should move to the next block to avoid infinite retries
                    pin.send_from_block += 1;
                    return Poll::Pending;
                }
                Poll::Ready(_) => return Poll::Pending,
                Poll::Pending => pin.msg_send_fut = Some(send_fut),
            }
        }

        Poll::Pending
    }
}

/// Returns an `Interval` from the given seconds.
fn interval_from_seconds(secs: u64) -> Interval {
    let duration = Duration::from_secs(secs);
    let mut interval = interval_at(Instant::now() + duration, duration);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

fn trace_msg_to_l1_sent(messages: &[MessageToL1], hashes: &[String]) {
    assert_eq!(messages.len(), hashes.len());

    let hash_exec_str = format!("{:#064x}", super::starknet::HASH_EXEC);

    for (i, m) in messages.iter().enumerate() {
        let payload_str: Vec<String> = m.payload.iter().map(|f| format!("{:#x}", *f)).collect();

        let hash = &hashes[i];

        if hash == &hash_exec_str {
            let to_address = &payload_str[0];
            let selector = &payload_str[1];
            let payload_str = &payload_str[2..];

            #[rustfmt::skip]
            info!(
                target: LOG_TARGET,
                from_address = %m.from_address,
                to_address = %to_address,
                selector = %selector,
                payload = %payload_str.join(", "),
                "Message executed on settlement layer.",
            );

            continue;
        }

        // We check for magic value 'MSG' used only when we are doing L3-L2 messaging.
        let (to_address, payload_str) = if format!("{}", m.to_address) == "0x4d5347" {
            (payload_str[0].clone(), &payload_str[1..])
        } else {
            (format!("{:#64x}", m.to_address), &payload_str[..])
        };

        #[rustfmt::skip]
            info!(
                target: LOG_TARGET,
                hash = %hash.as_str(),
                from_address = %m.from_address,
                to_address = %to_address,
                payload = %payload_str.join(", "),
                "Message sent to settlement layer.",
            );
    }
}

fn trace_l1_handler_tx_exec(hash: TxHash, tx: &L1HandlerTx) {
    let calldata_str: Vec<_> = tx.calldata.iter().map(|f| format!("{f:#x}")).collect();

    #[rustfmt::skip]
    info!(
        target: LOG_TARGET,
        tx_hash = %format!("{:#x}", hash),
        contract_address = %tx.contract_address,
        selector = %format!("{:#x}", tx.entry_point_selector),
        calldata = %calldata_str.join(", "),
        "L1Handler transaction added to the pool.",
    );
}
