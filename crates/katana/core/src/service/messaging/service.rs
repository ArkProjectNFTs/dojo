use crate::hooker::KatanaHooker;
use tokio::sync::RwLock as AsyncRwLock;

use futures::{Future, FutureExt, Stream};
use katana_executor::ExecutorFactory;
use katana_primitives::block::BlockHashOrNumber;
use katana_primitives::receipt::MessageToL1;
use katana_primitives::transaction::{ExecutableTxWithHash, L1HandlerTx, TxHash};
use katana_provider::traits::block::BlockNumberProvider;
use katana_provider::traits::transaction::ReceiptProvider;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
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
    /// The messaging configuration.
    messaging_config: Arc<RwLock<MessagingConfig>>,
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
        let gather_from_block = config.gather_from_block;
        let send_from_block = config.send_from_block;
        let interval = interval_from_seconds(config.interval);
        let messenger = match MessengerMode::from_config(config.clone(), hooker).await {
            Ok(m) => Arc::new(m),
            Err(_) => {
                panic!(
                    "Messaging could not be initialized.\nVerify that the messaging target node \
                     (anvil or other katana) is running.\n"
                )
            }
        };

        Ok(Self {
            pool,
            backend,
            interval,
            messenger,
            gather_from_block,
            send_from_block,
            msg_gather_fut: None,
            msg_send_fut: None,
            messaging_config: Arc::new(RwLock::new(config)),
        })
    }

    async fn gather_messages(
        messenger: Arc<MessengerMode<EF>>,
        pool: Arc<TransactionPool>,
        backend: Arc<Backend<EF>>,
        from_block: u64,
    ) -> MessengerResult<(u64, usize)> {
        // 200 avoids any possible rejection from RPC with possibly lots of messages.
        // TODO: May this be configurable?
        let max_block = 200;

        match messenger.as_ref() {
            MessengerMode::Ethereum(inner) => {
                let (block_num, txs) =
                    inner.gather_messages(from_block, max_block, backend.chain_id).await?;
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
                    inner.gather_messages(from_block, max_block, backend.chain_id).await?;
                let txs_count = txs.len();
                info!(target: LOG_TARGET, "Gathered {} transactions for Starknet mode.", txs_count);
                txs.into_iter().for_each(|tx| {
                    let hash = tx.calculate_hash();
                    info!(target: LOG_TARGET, "Processing transaction with hash: {:#x}", hash);
                    trace_l1_handler_tx_exec(hash, &tx);
                    pool.add_transaction(ExecutableTxWithHash { hash, transaction: tx.into() })
                });

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

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.get_mut();

        if pin.interval.poll_tick(cx).is_ready() {
            if pin.msg_gather_fut.is_none() {
                pin.msg_gather_fut = Some(Box::pin(Self::gather_messages(
                    pin.messenger.clone(),
                    pin.pool.clone(),
                    pin.backend.clone(),
                    pin.gather_from_block,
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

        let mut messaging_config = pin.messaging_config.write().unwrap();
        // Poll the gathering future
        if let Some(mut gather_fut) = pin.msg_gather_fut.take() {
            match gather_fut.poll_unpin(cx) {
                Poll::Ready(Ok((last_block, msg_count))) => {
                    info!(target: LOG_TARGET, "Gathered {} transactions up to block {}", msg_count, last_block);
                    pin.gather_from_block = last_block + 1;
                    messaging_config.gather_from_block = pin.gather_from_block;
                    let _ = messaging_config.save();
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
                    // update the config with the latest block number sent.
                    messaging_config.send_from_block = pin.send_from_block;
                    let _ = messaging_config.save();
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
