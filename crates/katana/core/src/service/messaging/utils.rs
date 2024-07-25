use std::fs::OpenOptions;
use std::io::{self, Read, Write, Seek};
use serde_json::Value;
use alloy_primitives::B256;
use starknet_api::hash::PoseidonHash;



/// Updates the last sent block number in the JSON file.
pub fn update_l2_block(new_block_num: u64, path : String) {
    //let path = "../../../../contracts/messaging/anvil.messaging.json";
    
   // Open the file with options to read and write without truncating.
   let mut file = OpenOptions::new()
   .read(true)
   .write(true)
   .open(path).expect("msg");

    // Read the existing content into a string.
    let mut contents = String::new();
    file.read_to_string(&mut contents).expect("msg");

    // Parse the JSON string into a serde_json::Value object.
    let mut data: Value = serde_json::from_str(&contents).expect("msg");

    // Update the 'from_block' field with the new block number.
    if let Some(obj) = data.as_object_mut() {
    obj.insert("gather_from_block".to_string(), serde_json::json!(new_block_num));
    //obj.insert("tx_hash".to_string(), serde_json::json!(tx_hash));
    }

    // Move back to the start of the file and truncate it to zero length.
    file.set_len(0).expect("msg");
    file.seek(io::SeekFrom::Start(0)).expect("msg");

    // Write the updated JSON back to the file.
    write!(file, "{}", data).expect("msg");
}

/// Updates the last sent block number in the JSON file.
pub fn update_l2_hash(tx_hash : B256, path : String) {
    //let path = "../../../../contracts/messaging/anvil.messaging.json";
    
   // Open the file with options to read and write without truncating.
   let mut file = OpenOptions::new()
   .read(true)
   .write(true)
   .open(path).expect("msg");

    // Read the existing content into a string.
    let mut contents = String::new();
    file.read_to_string(&mut contents).expect("msg");

    // Parse the JSON string into a serde_json::Value object.
    let mut data: Value = serde_json::from_str(&contents).expect("msg");

    // Update the 'from_block' field with the new block number.
    if let Some(obj) = data.as_object_mut() {
    //obj.insert("from_block".to_string(), serde_json::json!(new_block_num));
    obj.insert("tx_hash".to_string(), serde_json::json!(tx_hash));
    }

    // Move back to the start of the file and truncate it to zero length.
    file.set_len(0).expect("msg");
    file.seek(io::SeekFrom::Start(0)).expect("msg");

    // Write the updated JSON back to the file.
    write!(file, "{}", data).expect("msg");
}


/// Updates the last sent block number in the JSON file.
pub fn update_l3(new_block_num: u64, /*msg_hash : PoseidonHash,*/ path : String) {
    //let path = "../../../../contracts/messaging/l3.messaging.json";
    
   // Open the file with options to read and write without truncating.
   let mut file = OpenOptions::new()
   .read(true)
   .write(true)
   .open(path).expect("msg");

    // Read the existing content into a string.
    let mut contents = String::new();
    file.read_to_string(&mut contents).expect("msg");

    // Parse the JSON string into a serde_json::Value object.
    let mut data: Value = serde_json::from_str(&contents).expect("msg");

    // Update the 'from_block' field with the new block number.
    if let Some(obj) = data.as_object_mut() {
    obj.insert("send_from_block".to_string(), serde_json::json!(new_block_num));
    //obj.insert("msg_hash".to_string(), serde_json::json!(msg_hash));
    }

    // Move back to the start of the file and truncate it to zero length.
    file.set_len(0).expect("msg");
    file.seek(io::SeekFrom::Start(0)).expect("msg");

    // Write the updated JSON back to the file.
    write!(file, "{}", data).expect("msg");

}