//! Platform-independent core of catguard: key geometry, paw detection, the
//! lock state machine and the deterrent sound. Everything here runs and is
//! tested on any OS. The Windows shell lives in the binary.

pub mod detector;
pub mod guard;
pub mod history;
pub mod layout;
pub mod sound;
