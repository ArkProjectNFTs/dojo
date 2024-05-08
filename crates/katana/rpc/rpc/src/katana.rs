use crate::config::ServerConfig;
use base64::decode;
use jsonrpsee::core::{async_trait, Error};
use katana_core::accounts::Account;
use katana_core::hooker::HookerAddresses;
use katana_core::sequencer::KatanaSequencer;
use katana_primitives::FieldElement;
use katana_rpc_api::katana::KatanaApiServer;
use katana_rpc_types::error::katana::KatanaApiError;
use std::sync::Arc;

pub struct KatanaApi {
    sequencer: Arc<KatanaSequencer>,
    config: ServerConfig,
}

impl KatanaApi {
    pub fn new(sequencer: Arc<KatanaSequencer>, config: ServerConfig) -> Self {
        Self { sequencer, config }
    }

    fn verify_basic_auth(&self, encoded_credentials: &str) -> bool {
        if let Ok(credentials) = decode(encoded_credentials) {
            if let Ok(credentials_str) = String::from_utf8(credentials) {
                let parts: Vec<&str> = credentials_str.split(':').collect();
                if parts.len() == 2 {
                    let (username, password) = (parts[0], parts[1]);
                    return username == self.config.rpc_user
                        && password == self.config.rpc_password;
                }
            }
        }
        false
    }
}

#[async_trait]
impl KatanaApiServer for KatanaApi {
    async fn set_addresses(
        &self,
        addresses: HookerAddresses,
        basic_auth: String,
    ) -> Result<(), Error> {
        if !self.verify_basic_auth(&basic_auth) {
            panic!("authentication failed");
        }

        self.sequencer.set_addresses(addresses).await;
        Ok(())
    }

    async fn generate_block(&self) -> Result<(), Error> {
        self.sequencer.block_producer().force_mine();
        Ok(())
    }

    async fn next_block_timestamp(&self) -> Result<u64, Error> {
        // Ok(self.sequencer.backend().env.read().block.block_timestamp.0)
        unimplemented!()
    }

    async fn set_next_block_timestamp(&self, timestamp: u64) -> Result<(), Error> {
        self.sequencer
            .set_next_block_timestamp(timestamp)
            .map_err(|_| Error::from(KatanaApiError::FailedToChangeNextBlockTimestamp))
    }

    async fn increase_next_block_timestamp(&self, timestamp: u64) -> Result<(), Error> {
        self.sequencer
            .increase_next_block_timestamp(timestamp)
            .map_err(|_| Error::from(KatanaApiError::FailedToChangeNextBlockTimestamp))
    }

    async fn predeployed_accounts(&self) -> Result<Vec<Account>, Error> {
        Ok(self.sequencer.backend().accounts.clone())
    }

    async fn set_storage_at(
        &self,
        _contract_address: FieldElement,
        _key: FieldElement,
        _value: FieldElement,
    ) -> Result<(), Error> {
        // self.sequencer
        //     .set_storage_at(contract_address.into(), key, value)
        //     .await
        //     .map_err(|_| Error::from(KatanaApiError::FailedToUpdateStorage))
        Ok(())
    }
}
