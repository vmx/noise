extern crate capnp;
extern crate rocksdb;

use std::{error, fmt};

#[derive(Debug)]
pub enum Error {
    Parse(String),
    Shred(String),
    Capnp(capnp::Error),
    Rocks(rocksdb::Error),
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Parse(ref description) => description,
            Error::Shred(ref description) => description,
            Error::Capnp(ref err) => err.description(),
            // XXX vmx 2016-11-07: It should be fixed on the RocksDB wrapper
            // that it has the std::error:Error implemented and hence
            // and err.description()
            Error::Rocks(_) => "This is an rocksdb error",
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::Parse(_) => None,
            Error::Shred(_) => None,
            Error::Capnp(ref err) => Some(err as &error::Error),
            // NOTE vmx 2016-11-07: Looks like the RocksDB Wrapper needs to be
            // patched to be based on the std::error::Error trait
            Error::Rocks(_) => None,
        }
    }
}

impl From<capnp::Error> for Error {
    fn from(err: capnp::Error) -> Error {
        Error::Capnp(err)
    }
}

impl From<rocksdb::Error> for Error {
    fn from(err: rocksdb::Error) -> Error {
        Error::Rocks(err)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Parse(ref err) => write!(f, "Parse error: {}", err),
            Error::Shred(ref err) => write!(f, "Shred error: {}", err),
            Error::Capnp(ref err) => write!(f, "Capnproto error: {}", err),
            Error::Rocks(ref err) => write!(f, "RocksDB error: {}", err),
        }
    }
}
