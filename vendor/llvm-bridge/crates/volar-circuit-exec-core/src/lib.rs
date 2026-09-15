#![no_std]
//! Shared primitives for frontends that turn bounded program execution into
//! a Boolean circuit.
//!
//! The crate deliberately knows neither LLVM, WASM, nor a concrete circuit
//! representation.  A frontend supplies a [`CircuitEmitter`] for value
//! operations and uses [`StaticControlPlan`] to prove that loop/control-state
//! expansion is finite before it emits a control-free program.

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::{collections::BTreeSet, vec::Vec};

/// How a frontend handles a branch whose condition is not public.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ControlMode {
    /// Preserve the historical bounded-execution contract: branch conditions
    /// and loop exits must be concrete.
    #[default]
    Concrete,
    /// Emit both structured branch paths and join their values with MUXes.
    /// Loops still require a statically finite expansion plan.
    Predicated,
}

/// The small value-emission seam shared by source frontends.
///
/// All wider arithmetic is intentionally expressed in terms of these Boolean
/// operations by the owning frontend.  Keeping this interface small lets a
/// Volar `VCircuit`, Cirrus recorder, or plain `bool` interpreter share the
/// same control expansion without a dependency on another representation.
pub trait CircuitEmitter {
    /// The backend's Boolean wire handle.
    type Wire: Clone;
    /// A backend-emission failure.
    type Error;

    /// Materialize a Boolean constant.
    fn constant(&mut self, value: bool) -> Result<Self::Wire, Self::Error>;
    /// Emit conjunction.
    fn and(&mut self, left: Self::Wire, right: Self::Wire) -> Result<Self::Wire, Self::Error>;
    /// Emit disjunction.
    fn or(&mut self, left: Self::Wire, right: Self::Wire) -> Result<Self::Wire, Self::Error>;
    /// Emit exclusive-or.
    fn xor(&mut self, left: Self::Wire, right: Self::Wire) -> Result<Self::Wire, Self::Error>;
    /// Select `then_value` when `condition` is set, otherwise `else_value`.
    fn mux(
        &mut self,
        condition: Self::Wire,
        then_value: Self::Wire,
        else_value: Self::Wire,
    ) -> Result<Self::Wire, Self::Error>;
}

/// The AND/XOR subset needed by the canonical Boolean select identity.
///
/// It is deliberately separate from [`CircuitEmitter`] so allocator-free
/// runtime interpreters that receive their zero/one wires from their ABI can
/// share selection lowering without inventing a constant constructor.
pub trait SelectEmitter {
    /// The backend's Boolean wire handle.
    type Wire: Clone;
    /// A backend-emission failure.
    type Error;

    /// Emit conjunction.
    fn and(&mut self, left: Self::Wire, right: Self::Wire) -> Result<Self::Wire, Self::Error>;
    /// Emit exclusive-or.
    fn xor(&mut self, left: Self::Wire, right: Self::Wire) -> Result<Self::Wire, Self::Error>;
}

impl<T: CircuitEmitter> SelectEmitter for T {
    type Wire = T::Wire;
    type Error = T::Error;

    fn and(&mut self, left: Self::Wire, right: Self::Wire) -> Result<Self::Wire, Self::Error> {
        CircuitEmitter::and(self, left, right)
    }

    fn xor(&mut self, left: Self::Wire, right: Self::Wire) -> Result<Self::Wire, Self::Error> {
        CircuitEmitter::xor(self, left, right)
    }
}

/// Select `then_value` when `condition` is set, otherwise `else_value`.
///
/// This expands to `else ^ (condition & (then ^ else))`, which maps equally
/// well to a native MUX backend and to ERT's AND/XOR-only contexts.
pub fn select<E: SelectEmitter>(
    emitter: &mut E,
    condition: E::Wire,
    then_value: E::Wire,
    else_value: E::Wire,
) -> Result<E::Wire, E::Error> {
    let difference = emitter.xor(then_value, else_value.clone())?;
    let masked = emitter.and(condition, difference)?;
    emitter.xor(else_value, masked)
}

/// Emit `!value` through the common Boolean basis.
pub fn not<E: CircuitEmitter>(emitter: &mut E, value: E::Wire) -> Result<E::Wire, E::Error> {
    let one = emitter.constant(true)?;
    emitter.xor(value, one)
}

/// Produce the guards for the then and else side of a predicated branch.
pub fn branch_guards<E: CircuitEmitter>(
    emitter: &mut E,
    active: E::Wire,
    condition: E::Wire,
) -> Result<(E::Wire, E::Wire), E::Error> {
    let then_guard = emitter.and(active.clone(), condition.clone())?;
    let inverse = not(emitter, condition)?;
    let else_guard = emitter.and(active, inverse)?;
    Ok((then_guard, else_guard))
}

/// Lower a conditional storage update to read-modify-write form.
///
/// This is the side-effect counterpart of a SSA join.  It remains correct if
/// inactive paths compute aliases of an active path's address: the later
/// inactive write simply stores the value it just read.
pub fn guarded_write<E, Read, Write>(
    emitter: &mut E,
    guard: E::Wire,
    value: E::Wire,
    mut read: Read,
    mut write: Write,
) -> Result<(), E::Error>
where
    E: CircuitEmitter,
    Read: FnMut() -> Result<E::Wire, E::Error>,
    Write: FnMut(E::Wire) -> Result<(), E::Error>,
{
    let previous = read()?;
    let selected = guarded_value(emitter, guard, value, previous)?;
    write(selected)
}

/// Select a tentative side-effect value over the value observed before the
/// effect. This is the storage-independent core of [`guarded_write`].
pub fn guarded_value<E: CircuitEmitter>(
    emitter: &mut E,
    guard: E::Wire,
    value: E::Wire,
    previous: E::Wire,
) -> Result<E::Wire, E::Error> {
    emitter.mux(guard, value, previous)
}

/// Resource limits for static expansion.  These caps protect compiler
/// resources only; they never change the execution semantics of a circuit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlLimits {
    /// Maximum distinct static control states visited by one expansion.
    pub max_states: usize,
    /// Maximum source steps admitted to an expansion plan.
    pub max_steps: usize,
    /// Maximum nested direct-call depth.
    pub max_call_depth: usize,
}

impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            max_states: 4096,
            max_steps: 100_000,
            max_call_depth: 256,
        }
    }
}

/// Why a source frontend cannot form an exact finite circuit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlError {
    /// A static state repeated before the active path reached a return.
    NonFiniteControl,
    /// A source call cycle would require recursive inlining.
    RecursiveCall,
    /// One of [`ControlLimits`] was exceeded.
    ResourceLimit,
}

/// A deterministic static-control exploration.
///
/// The caller chooses a key containing every public discriminator required to
/// distinguish a control state, usually `(function, block-or-pc, values...)`.
/// Repeating an entered key is an exact non-termination signal for a
/// control-free importer; a caller must not reinterpret it as an unroll
/// count.
#[cfg(feature = "alloc")]
#[derive(Clone, Debug)]
pub struct StaticControlPlan<Key: Ord + Clone> {
    limits: ControlLimits,
    states: BTreeSet<Key>,
    order: Vec<Key>,
    call_depth: usize,
}

#[cfg(feature = "alloc")]
impl<Key: Ord + Clone> StaticControlPlan<Key> {
    /// Start an empty expansion plan.
    pub fn new(limits: ControlLimits) -> Self {
        Self {
            limits,
            states: BTreeSet::new(),
            order: Vec::new(),
            call_depth: 0,
        }
    }

    /// Record the next exact static state.
    pub fn enter(&mut self, state: Key) -> Result<(), ControlError> {
        if self.order.len() >= self.limits.max_steps || self.states.len() >= self.limits.max_states
        {
            return Err(ControlError::ResourceLimit);
        }
        if !self.states.insert(state.clone()) {
            return Err(ControlError::NonFiniteControl);
        }
        self.order.push(state);
        Ok(())
    }

    /// Enter one direct-call frame.  Frontends also use their own function
    /// stack to attach a human-readable call trace to [`ControlError`].
    pub fn enter_call(&mut self) -> Result<(), ControlError> {
        if self.call_depth >= self.limits.max_call_depth {
            return Err(ControlError::ResourceLimit);
        }
        self.call_depth += 1;
        Ok(())
    }

    /// Leave one direct-call frame.
    pub fn leave_call(&mut self) {
        self.call_depth = self.call_depth.saturating_sub(1);
    }

    /// The finite execution order proven so far.
    pub fn order(&self) -> &[Key] {
        &self.order
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[derive(Default)]
    struct BoolEmitter;

    impl CircuitEmitter for BoolEmitter {
        type Wire = bool;
        type Error = core::convert::Infallible;

        fn constant(&mut self, value: bool) -> Result<bool, Self::Error> {
            Ok(value)
        }
        fn and(&mut self, left: bool, right: bool) -> Result<bool, Self::Error> {
            Ok(left & right)
        }
        fn or(&mut self, left: bool, right: bool) -> Result<bool, Self::Error> {
            Ok(left | right)
        }
        fn xor(&mut self, left: bool, right: bool) -> Result<bool, Self::Error> {
            Ok(left ^ right)
        }
        fn mux(
            &mut self,
            condition: bool,
            then_value: bool,
            else_value: bool,
        ) -> Result<bool, Self::Error> {
            Ok(if condition { then_value } else { else_value })
        }
    }

    #[test]
    fn branch_guards_partition_an_active_path() {
        let mut emitter = BoolEmitter;
        assert_eq!(
            branch_guards(&mut emitter, true, true).unwrap(),
            (true, false)
        );
        assert_eq!(
            branch_guards(&mut emitter, true, false).unwrap(),
            (false, true)
        );
        assert_eq!(
            branch_guards(&mut emitter, false, true).unwrap(),
            (false, false)
        );
    }

    #[test]
    fn canonical_select_matches_its_truth_table() {
        let mut emitter = BoolEmitter;
        for condition in [false, true] {
            for then_value in [false, true] {
                for else_value in [false, true] {
                    assert_eq!(
                        select(&mut emitter, condition, then_value, else_value).unwrap(),
                        if condition { then_value } else { else_value },
                    );
                }
            }
        }
    }

    #[test]
    fn guarded_write_keeps_the_inactive_value() {
        let mut emitter = BoolEmitter;
        let stored = core::cell::Cell::new(false);
        guarded_write(
            &mut emitter,
            false,
            true,
            || Ok(stored.get()),
            |value| {
                stored.set(value);
                Ok(())
            },
        )
        .unwrap();
        assert!(!stored.get());
    }

    #[test]
    #[cfg(feature = "alloc")]
    fn repeated_static_state_is_not_an_unroll_bound() {
        let mut plan = StaticControlPlan::new(ControlLimits::default());
        plan.enter((0u32, 0u32)).unwrap();
        assert_eq!(plan.enter((0, 0)), Err(ControlError::NonFiniteControl));
    }
}
