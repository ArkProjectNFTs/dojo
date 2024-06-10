use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use katana_core::hooker::HookerAddresses;

#[cfg_attr(not(feature = "client"), rpc(server, namespace = "solis"))]
#[cfg_attr(feature = "client", rpc(client, server, namespace = "solis"))]
pub trait SolisApi {
    #[method(name = "setSolisAddresses")]
    async fn set_addresses(&self, addresses: HookerAddresses, basic_auth: String) -> RpcResult<()>;
}
