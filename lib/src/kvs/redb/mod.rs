#![cfg(feature = "kv-redb")]

use crate::err::Error;
use crate::kvs::Key;
use crate::kvs::Val;
use futures::lock::Mutex;
use redb::{Database, ReadableTable, TableDefinition, WriteTransaction, ReadTransaction};
use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;


const TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("surreal_db");


#[macro_export]
macro_rules! safe_unwrap {
    ($e:expr) => {
        $e.map_err(|e| { Error::Ds(e.to_string()) })?
    };
}


pub enum TransactionType {
	Read(ReadTransaction<'static>),
	Write(WriteTransaction<'static>),
}

impl TransactionType {

	fn rollback(&self) {}

}

#[derive(Clone)]
pub struct Datastore {
	db: Pin<Arc<Database>>,
}

pub struct Transaction {
	// Is the transaction complete?
	ok: bool,
	// Is the transaction read+write?
	rw: bool,
	// The distributed datastore transaction
	tx: Arc<Mutex<Option<TransactionType>>>,
	// The read options containing the Snapshot
	// ro: ReadOptions,
	// the above, supposedly 'static, transaction actually points here, so keep the memory alive
	// note that this is dropped last, as it is declared last
	_db: Pin<Arc<Database>>,
}

impl Datastore {
	/// Open a new database
	pub async fn new(path: &str) -> Result<Datastore, Error> {
		Ok(Datastore {
			db: Arc::pin(safe_unwrap!(Database::create(path))),
		})
	}
	/// Start a new transaction
	pub async fn transaction(&self, write: bool, _: bool) -> Result<Transaction, Error> {

		let tx = match write {
			false => {
				let tx = unsafe {
					std::mem::transmute::<
					ReadTransaction<'_>,
					ReadTransaction<'static>,
					>(safe_unwrap!(self.db.begin_read()))
				};
				TransactionType::Read(tx)
			},
			true => {
				let tx = unsafe {
					std::mem::transmute::<
					WriteTransaction<'_>,
					WriteTransaction<'static>,
					>(safe_unwrap!(self.db.begin_write()))
				};
				TransactionType::Write(tx)
			}
		};
		Ok(Transaction {
			ok: false,
			rw: write,
			tx: Arc::new(Mutex::new(Some(tx))),
			_db: self.db.clone(),
		})
	}
}

impl Transaction {
	/// Check if closed
	pub fn closed(&self) -> bool {
		self.ok
	}
	/// Cancel a transaction
	pub async fn cancel(&mut self) -> Result<(), Error> {
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		// Mark this transaction as done
		self.ok = true;
		// Cancel this transaction
		match self.tx.lock().await.take() {
			Some(tx) => match tx {
				TransactionType::Read(_) => {},
				TransactionType::Write(write_transaction) => safe_unwrap!(write_transaction.abort()),
			},
			None => unreachable!(),
		};
		// Continue
		Ok(())
	}
	/// Commit a transaction
	pub async fn commit(&mut self) -> Result<(), Error> {
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		// Check to see if transaction is writable
		if !self.rw {
			return Err(Error::TxReadonly);
		}
		// Mark this transaction as done
		self.ok = true;
		// Cancel this transaction
		match self.tx.lock().await.take() {
			Some(tx) => match tx {
				TransactionType::Read(_) => unreachable!(),
				TransactionType::Write(write_transaction) => safe_unwrap!(write_transaction.commit()),
			}
			None => unreachable!(),
		};
		// Continue
		Ok(())
	}
	/// Check if a key exists
	pub async fn exi<K>(&mut self, key: K) -> Result<bool, Error>
	where
		K: Into<Key>,
	{
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		if let None = self.tx.lock().await.take() {
			unreachable!()
		}

		match self.tx.lock().await.as_ref().unwrap() {
			TransactionType::Read(read_transaction) => {
				let table = safe_unwrap!(read_transaction.open_table(TABLE));
				let result = safe_unwrap!(table.get(key.into().as_slice()));
				Ok(result.is_some())
			},
			TransactionType::Write(write_transaction) => {
				let table = safe_unwrap!(write_transaction.open_table(TABLE));
				let result = safe_unwrap!(table.get(key.into().as_slice()));
				Ok(result.is_some())
			}
		}
	}
	/// Fetch a key from the database
	pub async fn get<K>(&mut self, key: K) -> Result<Option<Val>, Error>
	where
		K: Into<Key>,
	{
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		if let None = self.tx.lock().await.take() {
			unreachable!()
		}

		match self.tx.lock().await.take().unwrap() {
			TransactionType::Read(read_transaction) => {
				let table = safe_unwrap!(read_transaction.open_table(TABLE));
				let mut result = safe_unwrap!(table.get(key.into().as_slice()));
				match result.as_mut() {
					Some(v) => Ok(Some(v.value().to_vec())),
					None => Ok(None),
				}
			},
			TransactionType::Write(write_transaction) => {
				let table = safe_unwrap!(write_transaction.open_table(TABLE));
				let mut result = safe_unwrap!(table.get(key.into().as_slice()));
				match result.as_mut() {
					Some(v) => Ok(Some(v.value().to_vec())),
					None => Ok(None),
				}
			}
		}
	}
	/// Insert or update a key in the database
	pub async fn set<K, V>(&mut self, key: K, val: V) -> Result<(), Error>
	where
		K: Into<Key>,
		V: Into<Val>,
	{
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		// Check to see if transaction is writable
		if !self.rw {
			return Err(Error::TxReadonly);
		}
		// Set the key
		match self.tx.lock().await.as_ref().unwrap() {
			TransactionType::Read(_) => unreachable!(),
			TransactionType::Write(write_transaction) => {
				let mut table = safe_unwrap!(write_transaction.open_table(TABLE));
				safe_unwrap!(table.insert(key.into().as_slice(), val.into().as_slice()));
			}
		}
		Ok(())
	}
	/// Insert a key if it doesn't exist in the database
	pub async fn put<K, V>(&mut self, key: K, val: V) -> Result<(), Error>
	where
		K: Into<Key>,
		V: Into<Val>,
	{
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		// Check to see if transaction is writable
		if !self.rw {
			return Err(Error::TxReadonly);
		}
		// Get the transaction
		let tx = self.tx.lock().await;
		let tx = tx.as_ref().unwrap();

		// Get the arguments
		let key = key.into();
		let val = val.into();

		// Set the key if empty
		match tx {
			TransactionType::Read(_) => unreachable!(),
			TransactionType::Write(write_transaction) => {
				let mut table = safe_unwrap!(write_transaction.open_table(TABLE));
				{
					let key_result = safe_unwrap!(table.get(key.as_slice()));
					if key_result.is_some() == true {
						return Err(Error::TxKeyAlreadyExists);
					}
				}
				safe_unwrap!(table.insert(key.as_slice(), val.as_slice()));
			}
		}
		Ok(())
	}
	/// Insert a key only if it matches a check value
	pub async fn putc<K, V>(&mut self, key: K, val: V, chk: Option<V>) -> Result<(), Error>
	where
		K: Into<Key>,
		V: Into<Val>,
	{
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		// Check to see if transaction is writable
		if !self.rw {
			return Err(Error::TxReadonly);
		}
		// Get the transaction
		let tx = self.tx.lock().await;
		let tx = tx.as_ref().unwrap();

		// Get the arguments
		let key = key.into();
		let val = val.into();
		let chk = chk.map(Into::into);

		match tx {
			TransactionType::Read(_) => unreachable!(),
			TransactionType::Write(write_transaction) => {
				let mut table = safe_unwrap!(write_transaction.open_table(TABLE));
				let key_result = safe_unwrap!(table.get(key.clone().as_slice()));

				// set the key if valid
				match (&key_result, &chk) {
					(Some(v), Some(w)) => {
						let vec_ref: Vec<u8> = v.value().to_vec();
						if &vec_ref == w {
							std::mem::drop(key_result);
							safe_unwrap!(table.insert(key.as_slice(), val.as_slice()));
						}
					},
					(None, None) => {
						std::mem::drop(key_result);
						safe_unwrap!(table.insert(key.as_slice(), val.as_slice()));
					},
					_ => return Err(Error::TxConditionNotMet),
				}
			}
		};
		Ok(())
	}
	/// Delete a key
	pub async fn del<K>(&mut self, key: K) -> Result<(), Error>
	where
		K: Into<Key>,
	{
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		// Check to see if transaction is writable
		if !self.rw {
			return Err(Error::TxReadonly);
		}
		// Remove the key
		match self.tx.lock().await.as_ref().unwrap() {
			TransactionType::Read(_) => unreachable!(),
			TransactionType::Write(write_transaction) => {
				let mut table = safe_unwrap!(write_transaction.open_table(TABLE));
				safe_unwrap!(table.remove(key.into().as_slice()));
			}
		}
		// Return result
		Ok(())
	}
	/// Delete a key
	pub async fn delc<K, V>(&mut self, key: K, chk: Option<V>) -> Result<(), Error>
	where
		K: Into<Key>,
		V: Into<Val>,
	{
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		// Check to see if transaction is writable
		if !self.rw {
			return Err(Error::TxReadonly);
		}
		// Get the transaction
		let tx = self.tx.lock().await;
		let tx = tx.as_ref().unwrap();
		// Get the arguments
		let key = key.into();
		let chk = chk.map(Into::into);


		// Delete the key if valid
		match tx {
			TransactionType::Read(_) => unreachable!(),
			TransactionType::Write(write_transaction) => {
				let table = safe_unwrap!(write_transaction.open_table(TABLE));
				let key_result = &table.get(key.clone().as_slice()).map_err(|e| { Error::Ds(e.to_string()) })?;

				// set the key if valid
				match (key_result, chk) {
					(Some(v), Some(w)) => {
						// std::mem::drop(key_result);
						let vec_ref: Vec<u8> = v.value().to_vec();
						if vec_ref == w {
							let mut table = safe_unwrap!(write_transaction.open_table(TABLE));
							safe_unwrap!(table.remove(key.as_slice()));
						}
					},
					(None, None) => {
						// std::mem::drop(key_result);
						let mut table = safe_unwrap!(write_transaction.open_table(TABLE));
						safe_unwrap!(table.remove(key.as_slice()));
					},
					_ => return Err(Error::TxConditionNotMet),
				}
			}
		};
		// Return result
		Ok(())
	}
	/// Retrieve a range of keys from the databases
	pub async fn scan<K>(&mut self, rng: Range<K>, limit: u32) -> Result<Vec<(Key, Val)>, Error>
	where
		K: Into<Key>,
	{
		// Check to see if transaction is closed
		if self.ok {
			return Err(Error::TxFinished);
		}
		// Get the transaction
		let tx = self.tx.lock().await;
		let tx = tx.as_ref().unwrap();
		// Convert the range to bytes
		let rng: Range<Key> = Range {
			start: rng.start.into(),
			end: rng.end.into(),
		};
		// Create result set
		let mut res = vec![];
		// Set the key range
		let beg = rng.start.as_slice();
		let end = rng.end.as_slice();

		let mut closure = |mut iter: redb::Range<'_, &[u8], &[u8]>| {
			loop {
				match iter.next() {
					Some(Ok((k, v))) => {
						// Check the scan limit
						if res.len() < limit as usize {
							// Get the key and value
							let (k, v) = (k.value(), v.value());
							// Check the key and value
							if k >= beg && k < end {
								res.push((k.to_vec(), v.to_vec()));
							}
						}
					}
					Some(Err(e)) => {
						return Err(Error::Ds(e.to_string()));
					}
					None => {
						return Ok(());
					}
				} 
			}
		};

		match tx {
			TransactionType::Read(read_transaction) => {
				let table = safe_unwrap!(read_transaction.open_table(TABLE));
				let generator = safe_unwrap!(table.range(rng.start.as_slice()..rng.end.as_slice()));
				closure(generator)?;
			},
			TransactionType::Write(write_transaction) => {
				let table = safe_unwrap!(write_transaction.open_table(TABLE));
				let generator = safe_unwrap!(table.range(rng.start.as_slice()..rng.end.as_slice()));
				closure(generator)?;
			}
		};
		// Return result
		Ok(res)
	}
}


#[cfg(test)]
mod tests {
	use crate::kvs::tests::transaction::verify_transaction_isolation;
	use temp_dir::TempDir;

	// https://github.com/surrealdb/surrealdb/issues/76
	#[tokio::test]
	async fn soundness() {
		let mut transaction = get_transaction().await;
		transaction.put("uh", "oh").await.unwrap();

		async fn get_transaction() -> crate::kvs::Transaction {
			let datastore = crate::kvs::Datastore::new("redb:/tmp/spee.db").await.unwrap();
			datastore.transaction(true, false).await.unwrap()
		}
	}

	#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
	async fn redb_transaction() {
		let p = TempDir::new().unwrap().path().to_string_lossy().to_string();
		verify_transaction_isolation(&format!("file:{}", p)).await;
	}
}