#![no_std]
#![feature(alloc_error_handler)]
#![feature(lang_items, start)]
use revm::{
    db::CacheState,
    interpreter::CreateScheme,
    primitives::{calc_excess_blob_gas, keccak256, Bytecode, Env, SpecId, TransactTo, TxKind, U256, B256, Address},
    Evm,
};
extern crate libc;

use models::*;

mod utils;

use utils::recover_address;

extern crate alloc;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use alloc::alloc::*;
use alloc::boxed::Box;
use core::panic::PanicInfo;
use core::ffi::c_void;

use libc::{free, malloc};

#[panic_handler]
fn panic(_panic: &PanicInfo<'_>) -> ! {
    loop {}
}

/// The global allocator type.
#[derive(Default)]
pub struct Allocator;

unsafe impl GlobalAlloc for Allocator {
     unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
         malloc(layout.size()) as *mut u8
     }
     unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
         free(ptr as *mut c_void);
     }
}

/// If there is an out of memory error, just panic.
#[alloc_error_handler]
fn my_allocator_error(_layout: Layout) -> ! {
    panic!("out of memory");
}

/// The static global allocator.
#[global_allocator]
static GLOBAL_ALLOCATOR: Allocator = Allocator;

#[start]
fn main(_argc: isize, _argv: *const *const u8) -> isize {
    ethereum_test();
    0
}

fn ethereum_test() {
    let suite_json: String = r#"{
        "_info": null,
        "env": {
          "currentCoinbase": "0x0000000000000000000000000000000000000000",
          "currentDifficulty": "0x400000000",
          "currentGasLimit": "0x1388",
          "currentNumber": "0x0",
          "currentTimestamp": "0x0",
          "currentBaseFee": "0x3b9aca00",
          "previousHash": "0x0000000000000000000000000000000000000000000000000000000000000000",
          "currentRandom": "0x0000000000000000000000000000000000000000000000000000000000000000",
          "currentBeaconRoot": "0x0000000000000000000000000000000000000000000000000000000000000000",
          "currentWithdrawalsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
          "parentBlobGasUsed": "0x0",
          "parentExcessBlobGas": "0x0"
        },
        "pre": {},
        "post": {},
        "transaction": {
          "data": [],
          "gasLimit": [],
          "gasPrice": null,
          "nonce": "0x0",
          "secretKey": "0x0000000000000000000000000000000000000000000000000000000000000000",
          "sender": "0x0000000000000000000000000000000000000000",
          "to": "0x0000000000000000000000000000000000000000",
          "value": [],
          "maxFeePerGas": null,
          "maxPriorityFeePerGas": null,
          "accessLists": [],
          "blobVersionedHashes": [],
          "maxFeePerBlobGas": null
        },
        "out": null
      }"#.to_string();
    let suite = read_suite(&suite_json);

    assert!(execute_test_suite(suite).is_ok());
}

fn read_suite(s: &String) -> TestSuite {
    let suite: TestUnit = serde_json::from_str(s).map_err(|e| e).unwrap();
    let mut btm = BTreeMap::new();
    btm.insert("test".to_string(), suite);
    TestSuite(btm)
}

fn execute_test_suite(suite: TestSuite) -> Result<(), String> {
    for (_name, unit) in suite.0 {
        // Create database and insert cache
        let mut cache_state = CacheState::new(false);
        for (address, info) in unit.pre {
            let acc_info = revm::primitives::AccountInfo {
                balance: info.balance,
                code_hash: keccak256(&info.code),
                code: Some(Bytecode::new_raw(info.code)),
                nonce: info.nonce,
            };
            cache_state.insert_account_with_storage(address, acc_info, info.storage);
        }

        let mut env = Env::default();
        // for mainnet
        env.cfg.chain_id = 1;
        // env.cfg.spec_id is set down the road

        // block env
        env.block.number = unit.env.current_number;
        env.block.coinbase = unit.env.current_coinbase;
        env.block.timestamp = unit.env.current_timestamp;
        env.block.gas_limit = unit.env.current_gas_limit;
        env.block.basefee = unit.env.current_base_fee.unwrap_or_default();
        env.block.difficulty = unit.env.current_difficulty;
        // after the Merge prevrandao replaces mix_hash field in block and replaced difficulty opcode in EVM.
        env.block.prevrandao = unit.env.current_random;
        // EIP-4844
        if let (Some(parent_blob_gas_used), Some(parent_excess_blob_gas)) = (
            unit.env.parent_blob_gas_used,
            unit.env.parent_excess_blob_gas,
        ) {
            env.block
                .set_blob_excess_gas_and_price(calc_excess_blob_gas(
                    parent_blob_gas_used.to(),
                    parent_excess_blob_gas.to(),
                ));
        }

        // tx env
        env.tx.caller = match unit.transaction.sender {
            Some(address) => address,
            _ => recover_address(unit.transaction.secret_key.as_slice())
                .ok_or_else(|| String::new())?,
        };
        env.tx.gas_price = unit
            .transaction
            .gas_price
            .or(unit.transaction.max_fee_per_gas)
            .unwrap_or_default();
        env.tx.gas_priority_fee = unit.transaction.max_priority_fee_per_gas;
        // EIP-4844
        env.tx.blob_hashes = unit.transaction.blob_versioned_hashes;
        env.tx.max_fee_per_blob_gas = unit.transaction.max_fee_per_blob_gas;

        // post and execution
        for (spec_name, tests) in unit.post {
            if matches!(
                spec_name,
                SpecName::ByzantiumToConstantinopleAt5
                    | SpecName::Constantinople
                    | SpecName::Unknown
            ) {
                continue;
            }

            let spec_id = spec_name.to_spec_id();

            for (_index, test) in tests.into_iter().enumerate() {
                env.tx.gas_limit = unit.transaction.gas_limit[test.indexes.gas].saturating_to();

                env.tx.data = unit
                    .transaction
                    .data
                    .get(test.indexes.data)
                    .unwrap()
                    .clone();
                env.tx.value = unit.transaction.value[test.indexes.value];

                env.tx.access_list = unit
                    .transaction
                    .access_lists
                    .get(test.indexes.data)
                    .and_then(Option::as_deref)
                    .unwrap_or_default()
                    .iter()
                    .map(|item| {
                        let storage_keys: Vec<B256> = item.storage_keys
                            .iter()
                            .map(|h256| B256::new(h256.0))
                            .collect();
                        revm::primitives::AccessListItem {
                            address: Address::new(*item.address.0),
                            storage_keys,
                        }
                    })
                    .collect();

                let to = match unit.transaction.to {
                    Some(add) => TransactTo::Call(add),
                    None => TxKind::Create,
                };
                env.tx.transact_to = to;

                let mut cache = cache_state.clone();
                cache.set_state_clear_flag(SpecId::enabled(
                    spec_id,
                    revm::primitives::SpecId::SPURIOUS_DRAGON,
                ));
                let mut state = revm::db::State::builder()
                    .with_cached_prestate(cache)
                    .with_bundle_update()
                    .build();
                let mut evm = Evm::builder()
                    .with_db(&mut state)
                    .modify_env(|e| (*e) = Box::new(env.clone()))
                    .with_spec_id(spec_id)
                    .build();

                // do the deed
                //let timer = Instant::now();
                let mut check = || {
                    let exec_result = evm.transact_commit();

                    match (&test.expect_exception, &exec_result) {
                        // do nothing
                        (None, Ok(_)) => (),
                        // return okay, exception is expected.
                        (Some(_), Err(_e)) => {
                            return Ok(());
                        }
                        _ => {
                            let s = exec_result.clone().err().map(|e| e.to_string()).unwrap();
                            return Err(s);
                        }
                    }
                    Ok(())
                };

                let Err(e) = check() else { continue };

                return Err(e);
            }
        }
    }
    Ok(())
}
