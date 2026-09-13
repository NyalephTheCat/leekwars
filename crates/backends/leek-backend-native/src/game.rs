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
///
/// The runtime is moved *out* of the thread-local for the duration of the
/// call, so no `RefCell` borrow is held while it runs. A re-entrant game call
/// made from inside [`GameRuntime::call`] (e.g. the runtime invoking an AI
/// callback that itself calls a fight function) therefore sees no installed
/// runtime and yields `null`, instead of double-borrowing and panicking. The
/// runtime is put back when the call returns — or unwinds — unless a new one
/// was installed meanwhile.
pub(crate) fn dispatch(name: &str, args: &[Value]) -> Value {
    /// Reinstalls the taken runtime on drop (normal return or panic).
    struct Reinstall(Option<Box<dyn GameRuntime>>);
    impl Drop for Reinstall {
        fn drop(&mut self) {
            if let Some(rt) = self.0.take() {
                // `try_with`: the thread-local may already be destroyed if this
                // runs during thread teardown; the runtime is then just dropped.
                let _ = GAME.try_with(|g| {
                    let mut slot = g.borrow_mut();
                    if slot.is_none() {
                        *slot = Some(rt);
                    }
                });
            }
        }
    }

    let Some(rt) = GAME.with(|g| g.borrow_mut().take()) else {
        return Value::Null;
    };
    let mut taken = Reinstall(Some(rt));
    taken
        .0
        .as_mut()
        .map_or(Value::Null, |rt| rt.call(name, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runtime whose `outer` call re-enters [`dispatch`].
    struct Reentrant;

    impl GameRuntime for Reentrant {
        fn call(&mut self, name: &str, _args: &[Value]) -> Value {
            match name {
                "outer" => {
                    let inner = dispatch("inner", &[]);
                    Value::Int(if matches!(inner, Value::Null) { 1 } else { 2 })
                }
                _ => Value::Int(3),
            }
        }
    }

    #[test]
    fn reentrant_dispatch_does_not_double_borrow() {
        set_game_runtime(Some(Box::new(Reentrant)));
        // The nested call sees no runtime (null) rather than panicking on a
        // second `borrow_mut`.
        assert!(matches!(dispatch("outer", &[]), Value::Int(1)));
        // The runtime is reinstalled afterwards.
        assert!(matches!(dispatch("inner", &[]), Value::Int(3)));
        set_game_runtime(None);
        assert!(matches!(dispatch("inner", &[]), Value::Null));
    }
}
