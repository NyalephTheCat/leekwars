//! Leek-wars fight engine — the game-side equivalent of `leek-runtime`.
//!
//! Where `leek_runtime::call_builtin` implements the standard library against
//! a [`BuiltinHost`](leek_runtime), [`call_game_builtin`] implements the
//! leek-wars *fight* functions (`getCell`, `getLife`, `moveTowardCell`,
//! `useWeapon`, …) against a [`GameHost`]. This crate holds the **game
//! model**: the functions ([`builtins`]), the effect/stat model ([`effect`]),
//! the weapon/chip catalogs ([`weapons`], [`chips`]), the [`GameHost`] seam
//! ([`host`]), and the reference world it operates on ([`Entity`], [`Fight`]).
//! Fight **orchestration** — compiling and launching AIs, the turn loop —
//! lives in the generator crate, which drives a [`Fight`] through the native
//! backend.
//!
//! Keeping the functions behind [`GameHost`] means a function like
//! `getCellDistance` is written once, independent of how the world stores the
//! map — exactly the `leek-runtime` / backend split, one layer up.

pub mod builtins;
pub mod chips;
pub mod effect;
pub mod entity;
pub mod fight;
pub mod host;
pub mod weapons;

pub use builtins::{
    CELL_EMPTY, CELL_OBSTACLE, CELL_PLAYER, USE_CRITICAL, USE_FAILED, USE_INVALID_POSITION,
    USE_INVALID_TARGET, USE_NOT_ENOUGH_TP, USE_SUCCESS, USE_TOO_MANY_USES, call_game_builtin,
    is_game_builtin,
};
pub use effect::{ActiveEffect, Effect, EffectKind, Stat};
pub use entity::Entity;
pub use fight::{Fight, FightRef, shared};
pub use host::GameHost;
