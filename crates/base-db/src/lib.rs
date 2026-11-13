//! The incremental database's inputs: file texts, source roots, the module
//! graph and library roots. Every derived fact is a query over these, so an
//! edit invalidates exactly what depends on it.
