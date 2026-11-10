//! Deterministic inputs used only by the exact-wire characterization test.
//! Each test uses its own thread; normal crypto tests keep OS randomness.
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn active() -> bool {
    ACTIVE.with(Cell::get)
}

pub(crate) struct FixedInputs;

impl FixedInputs {
    pub(crate) fn enter() -> Self {
        ACTIVE.with(|active| assert!(!active.replace(true)));
        Self
    }
}

impl Drop for FixedInputs {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(false));
    }
}
