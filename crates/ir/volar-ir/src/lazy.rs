//! Chunked, content-addressed transport for fused Boolar circuits.
//!
//! A [`ChunkedBCircuit`] keeps only small root metadata in memory. Its
//! statement chunks are independently serialized and content-addressed, so a
//! runner can fetch, validate, decode, and discard one semantic range at a
//! time. A chunk is never split through an individual Boolar node.

use alloc::vec::Vec;
use core::fmt;

use lazy_repo::{ChunkCodec, ChunkRef, ChunkSink, DecodeError, DecodedChunk, Repository};
use volar_ir_common::Node;

use crate::{
    boolar::{BIrPreInitSegment, BIrStmt},
    circuit::BCircuit,
    ir::IRVarId,
};

/// Root metadata for a Boolar circuit whose statement body is loaded in
/// semantic ranges.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct ChunkedBCircuit {
    /// On-disk format revision.
    pub version: u32,
    /// Number of input bit parameters.
    pub params: u32,
    /// Bit-granular storage values installed before a fresh execution.
    pub pre_init: Vec<BIrPreInitSegment>,
    /// Variables returned to the caller.
    pub outputs: Vec<IRVarId>,
    /// Consecutive statement ranges in execution order.
    pub statement_chunks: Vec<BStmtChunkRef>,
    /// Target-specific bound and liveness data for out-of-core wire execution.
    pub wire_schedule: WireSchedule,
}

/// Build-time wire liveness data consumed by bounded lazy runners.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct WireSchedule {
    /// Maximum number of wire values the runner may retain in RAM.
    pub resident_wires: u32,
    /// Maximum temporary operand wires required by one Boolar statement.
    pub temporary_wires: u32,
    /// Last use position for each Boolar variable (parameters then results).
    /// Position zero precedes the first statement; each statement occupies
    /// `index + 1`; the final position preserves declared outputs.
    pub last_use: Vec<u32>,
    /// Variables that become dead after each position, including position zero.
    pub release_at: Vec<Vec<u32>>,
}

/// Why a requested target wire budget cannot run a circuit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireScheduleError {
    /// The target needs more room for one operation's inputs and output.
    InsufficientResidentWires {
        /// Number of simultaneously resident/temporary wires required.
        required: u32,
        /// Configured target budget.
        available: u32,
    },
    /// The circuit cannot be indexed with the on-disk `u32` representation.
    TooManyStatements,
}

impl WireSchedule {
    /// Build an immutable liveness schedule for one circuit and target budget.
    pub fn build<P: Clone>(
        circuit: &BCircuit<P>,
        resident_wires: u32,
    ) -> Result<Self, WireScheduleError> {
        let statement_count =
            u32::try_from(circuit.stmts.len()).map_err(|_| WireScheduleError::TooManyStatements)?;
        let var_count = circuit
            .params
            .checked_add(statement_count)
            .ok_or(WireScheduleError::TooManyStatements)?;
        let mut last_use = Vec::with_capacity(var_count as usize);
        for variable in 0..var_count {
            // Inputs that are never read can be discarded before execution;
            // dead statement results are discarded immediately after creation.
            last_use.push(if variable < circuit.params {
                0
            } else {
                variable - circuit.params + 1
            });
        }
        let mut temporary_wires = 0_u32;
        for (index, node) in circuit.stmts.iter().enumerate() {
            let position =
                u32::try_from(index + 1).map_err(|_| WireScheduleError::TooManyStatements)?;
            let inputs = statement_inputs(&node.kind);
            let operation_temporary_wires = match &node.kind {
                BIrStmt::Not(_) => 2,
                _ => {
                    u32::try_from(inputs.len()).map_err(|_| WireScheduleError::TooManyStatements)?
                }
            };
            temporary_wires = temporary_wires.max(operation_temporary_wires);
            for input in inputs {
                let Some(last) = last_use.get_mut(input.0 as usize) else {
                    // Leave malformed references for the execution layer to
                    // diagnose with its normal UndefinedVariable error.
                    continue;
                };
                *last = (*last).max(position);
            }
        }
        let output_position = statement_count.saturating_add(1);
        for output in &circuit.outputs {
            if let Some(last) = last_use.get_mut(output.0 as usize) {
                *last = (*last).max(output_position);
            }
        }
        let required = temporary_wires.saturating_add(1);
        if resident_wires < required {
            return Err(WireScheduleError::InsufficientResidentWires {
                required,
                available: resident_wires,
            });
        }
        let mut release_at = (0..=output_position)
            .map(|_| Vec::new())
            .collect::<Vec<_>>();
        for (variable, position) in last_use.iter().copied().enumerate() {
            release_at[position as usize].push(variable as u32);
        }
        Ok(Self {
            resident_wires,
            temporary_wires,
            last_use,
            release_at,
        })
    }

    /// Number of persistent value slots after reserving temporary operands.
    #[must_use]
    pub fn persistent_wires(&self) -> u32 {
        self.resident_wires.saturating_sub(self.temporary_wires)
    }
}

impl ChunkedBCircuit {
    /// Current serialized root format revision.
    pub const VERSION: u32 = 1;

    /// Total statement count described by the root metadata.
    #[must_use]
    pub fn statement_count(&self) -> u32 {
        self.statement_chunks
            .last()
            .map_or(0, |chunk| chunk.first.saturating_add(chunk.statements))
    }

    /// Fetch, digest-check, decode, and range-check one statement chunk.
    pub fn load_statement_chunk<S, C, P>(
        &self,
        repository: &mut Repository<S>,
        index: usize,
        codec: &C,
    ) -> Result<DecodedChunk<BStmtChunk<P>>, ChunkLoadError<S::Error, C::Error>>
    where
        S: lazy_repo::ChunkSource,
        C: ChunkCodec<BStmtChunk<P>>,
        P: Clone,
    {
        let expected = self
            .statement_chunks
            .get(index)
            .ok_or(ChunkLoadError::MissingChunk { index })?;
        let loaded = repository
            .decode(&expected.chunk, codec)
            .map_err(ChunkLoadError::Decode)?;
        if loaded.value.first != expected.first
            || loaded.value.stmts.len() != expected.statements as usize
        {
            return Err(ChunkLoadError::RangeMismatch {
                index,
                expected_first: expected.first,
                expected_statements: expected.statements,
                found_first: loaded.value.first,
                found_statements: loaded.value.stmts.len(),
            });
        }
        Ok(loaded)
    }

    /// Asynchronously fetch, validate, decode, and range-check one chunk.
    ///
    /// The future holds only this one decoded semantic range; callers can
    /// drop it before awaiting the next range.
    pub async fn load_statement_chunk_async<S, C, P>(
        &self,
        repository: &mut lazy_repo::AsyncRepository<S>,
        index: usize,
        codec: &C,
    ) -> Result<DecodedChunk<BStmtChunk<P>>, ChunkLoadError<S::Error, C::Error>>
    where
        S: lazy_repo::AsyncChunkSource,
        C: ChunkCodec<BStmtChunk<P>>,
        P: Clone,
    {
        let expected = self
            .statement_chunks
            .get(index)
            .ok_or(ChunkLoadError::MissingChunk { index })?;
        let loaded = repository
            .decode(&expected.chunk, codec)
            .await
            .map_err(ChunkLoadError::Decode)?;
        if loaded.value.first != expected.first
            || loaded.value.stmts.len() != expected.statements as usize
        {
            return Err(ChunkLoadError::RangeMismatch {
                index,
                expected_first: expected.first,
                expected_statements: expected.statements,
                found_first: loaded.value.first,
                found_statements: loaded.value.stmts.len(),
            });
        }
        Ok(loaded)
    }
}

/// A content reference plus the semantic statement range it contains.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct BStmtChunkRef {
    /// Content-addressed encoded chunk.
    pub chunk: ChunkRef,
    /// Index of the first statement in the circuit-wide sequence.
    pub first: u32,
    /// Number of complete statements in the chunk.
    pub statements: u32,
}

/// The decoded payload of one [`BStmtChunkRef`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct BStmtChunk<P: Clone> {
    /// Index of the first statement in the circuit-wide sequence.
    pub first: u32,
    /// Complete Boolar statement nodes in execution order.
    pub stmts: Vec<Node<BIrStmt, P>>,
}

/// Why an eager circuit could not be emitted as bounded lazy chunks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChunkingError<CodecError, SinkError> {
    /// The configured cap must leave room for at least one encoded node.
    ZeroChunkLimit,
    /// The circuit has too many statements to describe with `u32` indices.
    TooManyStatements,
    /// The target's resident-wire budget cannot execute this circuit.
    WireSchedule(WireScheduleError),
    /// Encoding a candidate range failed.
    Codec(CodecError),
    /// Persisting the final chunk failed.
    Sink(SinkError),
    /// One indivisible Boolar node exceeds the configured encoded-size cap.
    NodeTooLarge {
        /// Circuit-wide statement index.
        statement: u32,
        /// Encoded byte length of that single-node chunk.
        encoded_len: usize,
        /// Configured byte cap.
        max_chunk_bytes: usize,
    },
}

impl<C: fmt::Display, S: fmt::Display> fmt::Display for ChunkingError<C, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroChunkLimit => write!(f, "lazy Boolar chunk size must be non-zero"),
            Self::TooManyStatements => write!(f, "Boolar circuit exceeds u32 statement indexing"),
            Self::WireSchedule(error) => write!(f, "invalid Boolar wire schedule: {error:?}"),
            Self::Codec(error) => write!(f, "failed to encode Boolar chunk: {error}"),
            Self::Sink(error) => write!(f, "failed to store Boolar chunk: {error}"),
            Self::NodeTooLarge {
                statement,
                encoded_len,
                max_chunk_bytes,
            } => write!(
                f,
                "Boolar statement {statement} encodes to {encoded_len} bytes, above chunk cap {max_chunk_bytes}"
            ),
        }
    }
}

/// Why a chunk referenced by a [`ChunkedBCircuit`] could not be consumed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChunkLoadError<RepositoryError, CodecError> {
    /// The caller requested a chunk outside the manifest.
    MissingChunk { index: usize },
    /// Integrity checking or deserialization failed.
    Decode(DecodeError<RepositoryError, CodecError>),
    /// The decoded chunk does not match its root metadata range.
    RangeMismatch {
        /// Requested manifest index.
        index: usize,
        /// Expected first statement index.
        expected_first: u32,
        /// Expected number of statements.
        expected_statements: u32,
        /// First index supplied by decoded payload.
        found_first: u32,
        /// Number of decoded statements.
        found_statements: usize,
    },
}

/// Encode and store a circuit as contiguous semantic statement chunks.
///
/// `max_chunk_bytes` is measured after applying `codec`; it is not an
/// in-memory estimate. The function emits every chunk immediately and keeps
/// only the candidate range being sized in RAM.
pub fn chunk_b_circuit<P, C, S>(
    circuit: &BCircuit<P>,
    max_chunk_bytes: usize,
    resident_wires: u32,
    codec: &C,
    sink: &mut S,
) -> Result<ChunkedBCircuit, ChunkingError<C::Error, S::Error>>
where
    P: Clone,
    C: ChunkCodec<BStmtChunk<P>>,
    S: ChunkSink,
{
    if max_chunk_bytes == 0 {
        return Err(ChunkingError::ZeroChunkLimit);
    }
    let wire_schedule =
        WireSchedule::build(circuit, resident_wires).map_err(ChunkingError::WireSchedule)?;
    let _ = u32::try_from(circuit.stmts.len()).map_err(|_| ChunkingError::TooManyStatements)?;

    let mut statement_chunks = Vec::new();
    let mut first = 0_usize;
    while first < circuit.stmts.len() {
        let first_u32 = u32::try_from(first).map_err(|_| ChunkingError::TooManyStatements)?;
        let mut end = first;
        let mut candidate = Vec::new();
        loop {
            candidate.push(circuit.stmts[end].clone());
            let payload = BStmtChunk {
                first: first_u32,
                stmts: candidate.clone(),
            };
            let mut encoded = Vec::new();
            codec
                .encode(&payload, &mut encoded)
                .map_err(ChunkingError::Codec)?;
            if encoded.len() > max_chunk_bytes {
                if candidate.len() == 1 {
                    return Err(ChunkingError::NodeTooLarge {
                        statement: first_u32,
                        encoded_len: encoded.len(),
                        max_chunk_bytes,
                    });
                }
                candidate.pop();
                let payload = BStmtChunk {
                    first: first_u32,
                    stmts: candidate,
                };
                let mut encoded = Vec::new();
                codec
                    .encode(&payload, &mut encoded)
                    .map_err(ChunkingError::Codec)?;
                let statements = u32::try_from(payload.stmts.len())
                    .map_err(|_| ChunkingError::TooManyStatements)?;
                let chunk = sink.store(encoded).map_err(ChunkingError::Sink)?;
                statement_chunks.push(BStmtChunkRef {
                    chunk,
                    first: first_u32,
                    statements,
                });
                first += statements as usize;
                break;
            }
            end += 1;
            if end == circuit.stmts.len() {
                let statements =
                    u32::try_from(candidate.len()).map_err(|_| ChunkingError::TooManyStatements)?;
                let chunk = sink.store(encoded).map_err(ChunkingError::Sink)?;
                statement_chunks.push(BStmtChunkRef {
                    chunk,
                    first: first_u32,
                    statements,
                });
                first = end;
                break;
            }
        }
    }

    Ok(ChunkedBCircuit {
        version: ChunkedBCircuit::VERSION,
        params: circuit.params,
        pre_init: circuit.pre_init.clone(),
        outputs: circuit.outputs.clone(),
        statement_chunks,
        wire_schedule,
    })
}

fn statement_inputs(statement: &BIrStmt) -> Vec<IRVarId> {
    match statement {
        BIrStmt::And(left, right) | BIrStmt::Or(left, right) | BIrStmt::Xor(left, right) => {
            alloc::vec![*left, *right]
        }
        BIrStmt::Not(input) => alloc::vec![*input],
        BIrStmt::StorageRead { addr, .. } => addr.clone(),
        BIrStmt::StorageWrite { src, addr, .. } => {
            let mut inputs = Vec::with_capacity(addr.len().saturating_add(1));
            inputs.push(*src);
            inputs.extend_from_slice(addr);
            inputs
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use lazy_repo::{CacheConfig, ChunkCodec, MemorySource};

    #[derive(Clone, Copy)]
    struct NodeCountCodec;

    impl ChunkCodec<BStmtChunk<()>> for NodeCountCodec {
        type Error = core::convert::Infallible;

        fn encode(&self, value: &BStmtChunk<()>, out: &mut Vec<u8>) -> Result<(), Self::Error> {
            out.resize(value.stmts.len() * 10, 0);
            Ok(())
        }

        fn decode(&self, _bytes: &[u8]) -> Result<BStmtChunk<()>, Self::Error> {
            Ok(BStmtChunk {
                first: 0,
                stmts: Vec::new(),
            })
        }
    }

    #[test]
    fn chunks_on_complete_statement_boundaries() {
        let mut circuit = BCircuit::new(0);
        circuit.push_stmt(BIrStmt::Zero, ());
        circuit.push_stmt(BIrStmt::One, ());
        circuit.push_stmt(BIrStmt::Zero, ());
        let mut sink = MemorySource::default();
        let manifest = chunk_b_circuit(&circuit, 20, 3, &NodeCountCodec, &mut sink).unwrap();
        assert_eq!(manifest.statement_chunks.len(), 2);
        assert_eq!(manifest.statement_chunks[0].statements, 2);
        assert_eq!(manifest.statement_chunks[1].first, 2);
    }

    #[test]
    fn rejects_an_oversize_single_node() {
        let mut circuit = BCircuit::new(0);
        circuit.push_stmt(BIrStmt::Zero, ());
        let mut sink = MemorySource::default();
        assert!(matches!(
            chunk_b_circuit(&circuit, 9, 1, &NodeCountCodec, &mut sink),
            Err(ChunkingError::NodeTooLarge { .. })
        ));
    }

    #[test]
    fn loader_checks_the_declared_range() {
        let mut source = MemorySource::default();
        let chunk = source.insert(vec![0; 10]);
        let manifest = ChunkedBCircuit {
            version: 1,
            params: 0,
            pre_init: Vec::new(),
            outputs: Vec::new(),
            statement_chunks: vec![BStmtChunkRef {
                chunk,
                first: 2,
                statements: 1,
            }],
            wire_schedule: WireSchedule {
                resident_wires: 1,
                temporary_wires: 0,
                last_use: Vec::new(),
                release_at: vec![Vec::new()],
            },
        };
        let mut repository = Repository::new(
            source,
            CacheConfig {
                max_resident_bytes: 16,
                max_chunk_bytes: 16,
            },
        );
        assert!(matches!(
            manifest.load_statement_chunk(&mut repository, 0, &NodeCountCodec),
            Err(ChunkLoadError::RangeMismatch { .. })
        ));
    }
}
