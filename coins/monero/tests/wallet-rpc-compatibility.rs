use std::{
  collections::{HashSet, HashMap},
  str::FromStr,
};

use rand_core::{RngCore, OsRng};

use monero_rpc::{
  monero::{Amount, Address},
  TransferOptions,
};

use monero_serai::{
  wallet::address::{Network, AddressSpec, SubaddressIndex},
  wallet::Scanner,
};

mod runner;

async fn test_from_wallet_rpc_to_self(spec: AddressSpec) {
  let wallet_rpc =
    monero_rpc::RpcClientBuilder::new().build("http://127.0.0.1:6061").unwrap().wallet();
  let daemon_rpc = runner::rpc().await;

  // initialize wallet rpc
  let address_resp = wallet_rpc.get_address(0, None).await;
  let wallet_rpc_addr = if address_resp.is_ok() {
    address_resp.unwrap().address
  } else {
    wallet_rpc.create_wallet("test_wallet".to_string(), None, "English".to_string()).await.unwrap();
    let addr = wallet_rpc.get_address(0, None).await.unwrap().address;
    daemon_rpc.generate_blocks(&addr.to_string(), 70).await.unwrap();
    addr
  };

  // make an addr
  let (_, view_pair, _) = runner::random_address();
  let addr = Address::from_str(&view_pair.address(Network::Mainnet, spec).to_string()[..]).unwrap();

  // refresh & make a tx
  wallet_rpc.refresh(None).await.unwrap();
  let tx = wallet_rpc
    .transfer(
      HashMap::from([(addr, Amount::ONE_XMR)]),
      monero_rpc::TransferPriority::Default,
      TransferOptions::default(),
    )
    .await
    .unwrap();
  let tx_hash: [u8; 32] = tx.tx_hash.0.try_into().unwrap();

  // unlock it
  runner::mine_until_unlocked(&daemon_rpc, &wallet_rpc_addr.to_string(), tx_hash).await;

  // create the scanner
  let mut scanner = Scanner::from_view(view_pair, Some(HashSet::new()));
  if let AddressSpec::Subaddress(index) = spec {
    scanner.register_subaddress(index);
  }

  // retrieve it and confirm
  let tx = daemon_rpc.get_transaction(tx_hash).await.unwrap();
  let output = scanner.scan_transaction(&tx).not_locked().swap_remove(0);

  match spec {
    AddressSpec::Subaddress(index) => assert_eq!(output.metadata.subaddress, Some(index)),
    AddressSpec::Integrated(payment_id) => {
      assert_eq!(output.metadata.payment_id, payment_id);
      assert_eq!(output.metadata.subaddress, None);
    }
    _ => assert_eq!(output.metadata.subaddress, None),
  }
  assert_eq!(output.commitment().amount, 1000000000000);
}

async_sequential!(
  async fn test_receipt_of_wallet_rpc_tx_standard() {
    test_from_wallet_rpc_to_self(AddressSpec::Standard).await;
  }

  async fn test_receipt_of_wallet_rpc_tx_subaddress() {
    test_from_wallet_rpc_to_self(AddressSpec::Subaddress(SubaddressIndex::new(0, 1).unwrap()))
      .await;
  }

  async fn test_receipt_of_wallet_rpc_tx_integrated() {
    let mut payment_id = [0u8; 8];
    OsRng.fill_bytes(&mut payment_id);
    test_from_wallet_rpc_to_self(AddressSpec::Integrated(payment_id)).await;
  }
);