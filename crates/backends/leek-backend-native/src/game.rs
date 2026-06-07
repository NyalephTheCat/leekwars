//! Host game-library seam.
//!
//! The leek-wars fight functions (`getCell`, `getLife`, `say`, …) have no
//! body and no `@native-backend` directive — they're host functions provided
//! by the game engine. With [`crate::NativeOptions::link_game`] on, the
//! backend routes any otherwise-unknown builtin call to the
//! `leek_game_builtin` shim, which forwards to the [`GameRuntime`] installed
//! via [`set_game_runtime`].
//!
//! The runtime (and its fight state) lives in a separate crate (`leek-game`)
//! so the backend stays independent of the game model; it only knows "call
//! out by name." One game per thread — the JIT runs the program on a single
//! thread.

use std::cell::RefCell;

use leek_runtime::Value;

/// A host that implements the game (fight) builtins. Installed before running
/// a `link_game` program; the `leek_game_builtin` shim dispatches to it.
pub trait GameRuntime {
    /// Invoke the game function `name` with already-unboxed `args`, returning
    /// its value (`Value::Null` for an unknown or void function).
    fn call(&mut self, name: &str, args: &[Value]) -> Value;
}

thread_local! {
    static GAME: RefCell<Option<Box<dyn GameRuntime>>> = const { RefCell::new(None) };
}

/// Install (or clear, with `None`) the current thread's game runtime. Call
/// before running a `link_game`-compiled program; clear it afterward.
pub fn set_game_runtime(runtime: Option<Box<dyn GameRuntime>>) {
    GAME.with(|g| *g.borrow_mut() = runtime);
}

/// Dispatch a game builtin to the installed runtime. Returns `Value::Null`
/// when no runtime is installed (the function behaves as a no-op rather than
/// crashing the program).
pub(crate) fn dispatch(name: &str, args: &[Value]) -> Value {
    GAME.with(|g| {
        g.borrow_mut()
            .as_mut()
            .map_or(Value::Null, |rt| rt.call(name, args))
    })
}
