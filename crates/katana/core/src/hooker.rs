//! This module contains a hooker trait, that is added to katana in order to
//! allow external code to react at some precise moment of katana processing.

use crate::sequencer::KatanaSequencer;
use async_trait::async_trait;
use katana_executor::ExecutorFactory;
use starknet::accounts::Call;
use starknet::core::types::{BroadcastedInvokeTransaction, FieldElement};
use std::sync::Arc;
use tracing::{error, info};

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Copy, PartialEq, Eq)]
pub struct HookerAddresses {
    pub orderbook_arkchain: FieldElement,
    pub executor_starknet: FieldElement,
}

#[async_trait]
pub trait KatanaHooker<EF: ExecutorFactory> {
    /// Sets a reference to the underlying sequencer.
    fn set_sequencer(&mut self, sequencer: Arc<KatanaSequencer<EF>>);

    /// Runs code right before a message from the L1 is converted
    /// into a `L1HandlerTransaction`. This hook is useful to
    /// apply conditions on the message being captured.
    ///
    /// # Arguments
    ///
    /// * `from` - The contract on L2 sending the message.
    /// * `to` - The recipient contract on the appchain.
    /// * `selector` - The l1_handler of the appchain contract to execute.
    async fn verify_message_to_appchain(
        &self,
        from: FieldElement,
        to: FieldElement,
        selector: FieldElement,
    ) -> bool;

    /// Runs code right before an invoke transaction
    /// is being added to the pool.
    /// Returns true if the transaction should be included
    /// in the pool, false otherwise.
    ///
    /// # Arguments
    ///
    /// * `transaction` - The invoke transaction to be verified.
    async fn verify_invoke_tx_before_pool(&self, transaction: BroadcastedInvokeTransaction)
        -> bool;

    /// Runs code right before a message to starknet
    /// is being sent via a direct transaction.
    /// As the message is sent to starknet in a transaction
    /// the `Call` of the transaction is being verified.
    ///
    /// # Arguments
    ///
    /// * `call` - The `Call` to inspect, built from the
    /// message.
    async fn verify_tx_for_starknet(&self, call: Call) -> bool;

    /// Runs when Solis attempts to execute an order on Starknet,
    /// but it fails.
    ///
    /// # Arguments
    ///
    /// * `call` - The `Call` of the transaction that has failed. Usually the same as
    ///   `verify_message_to_starknet_before_tx`.
    async fn on_starknet_tx_failed(&self, call: Call);

    /// Sets important addresses.
    ///
    /// # Arguments
    ///
    /// * `addresses` - Important addresses related to solis.
    fn set_addresses(&mut self, addresses: HookerAddresses);
}

pub struct DefaultKatanaHooker<EF: ExecutorFactory> {
    sequencer: Option<Arc<KatanaSequencer<EF>>>,
    addresses: Option<HookerAddresses>,
}

impl<EF: ExecutorFactory> DefaultKatanaHooker<EF> {
    pub fn new() -> Self {
        DefaultKatanaHooker { sequencer: None, addresses: None }
    }
}

#[async_trait]
impl<EF: ExecutorFactory + 'static + Send + Sync> KatanaHooker<EF> for DefaultKatanaHooker<EF> {
    fn set_sequencer(&mut self, sequencer: Arc<KatanaSequencer<EF>>) {
        self.sequencer = Some(sequencer);
        info!("HOOKER: Sequencer set for hooker");
    }

    async fn verify_message_to_appchain(
        &self,
        from: FieldElement,
        to: FieldElement,
        selector: FieldElement,
    ) -> bool {
        info!(
            "HOOKER: verify_message_to_appchain called with from: {:?}, to: {:?}, selector: {:?}",
            from, to, selector
        );
        true
    }

    async fn verify_invoke_tx_before_pool(
        &self,
        transaction: BroadcastedInvokeTransaction,
    ) -> bool {
        info!("HOOKER: verify_invoke_tx_before_pool called with transaction: {:?}", transaction);
        true
    }

    async fn verify_tx_for_starknet(&self, call: Call) -> bool {
        info!("HOOKER: verify_tx_for_starknet called with call: {:?}", call);
        true
    }

    async fn on_starknet_tx_failed(&self, call: Call) {
        // Log the failure or handle it according to your needs. No-op by default.
        error!("HOOKER: Starknet transaction failed: {:?}", call);
    }

    fn set_addresses(&mut self, addresses: HookerAddresses) {
        self.addresses = Some(addresses);
        info!("HOOKER: Addresses set for hooker: {:?}", addresses);
    }
}
