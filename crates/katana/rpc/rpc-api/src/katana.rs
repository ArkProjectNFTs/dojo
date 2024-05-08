use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use katana_core::accounts::Account;
use katana_core::hooker::HookerAddresses;
use katana_primitives::FieldElement;

#[cfg_attr(not(feature = "client"), rpc(server, namespace = "katana"))]
#[cfg_attr(feature = "client", rpc(client, server, namespace = "katana"))]
pub trait KatanaApi {
    #[method(name = "setSolisAddresses")]
    async fn set_addresses(&self, addresses: HookerAddresses, basic_auth: String) -> RpcResult<()>;

    #[method(name = "generateBlock")]
    async fn generate_block(&self) -> RpcResult<()>;

    #[method(name = "nextBlockTimestamp")]
    async fn next_block_timestamp(&self) -> RpcResult<u64>;

    #[method(name = "setNextBlockTimestamp")]
    async fn set_next_block_timestamp(&self, timestamp: u64) -> RpcResult<()>;

    #[method(name = "increaseNextBlockTimestamp")]
    async fn increase_next_block_timestamp(&self, timestamp: u64) -> RpcResult<()>;

    #[method(name = "predeployedAccounts")]
    async fn predeployed_accounts(&self) -> RpcResult<Vec<Account>>;

    #[method(name = "setStorageAt")]
    async fn set_storage_at(
        &self,
        contract_address: FieldElement,
        key: FieldElement,
        value: FieldElement,
    ) -> RpcResult<()>;
}
