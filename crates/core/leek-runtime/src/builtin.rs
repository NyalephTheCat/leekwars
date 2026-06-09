//! Scalar built-in implementations shared by the interpreter and the
//! native (Cranelift) backend.
//!
//! These are the pure, stateless math builtins (`sqrt`, `cos`, `floor`,
//! …): each takes scalar `f64` argument(s) and returns a scalar, with no
//! operation metering, RNG, or game-API access. The native backend calls these
//! *same* functions so the semantics can't drift — it emits a Cranelift `call`
//! to them as registered JIT symbols.
//!
//! Functions are `extern "C"` so the native backend can call them across
//! the FFI boundary with a stable ABI. Their addresses are exposed via
//! [`math_builtins`] for symbol registration.

/// The scalar signature of a math builtin — drives the Cranelift
/// signature the native backend declares and the result kind it expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathSig {
    /// `f64 -> f64`.
    RealToReal,
    /// `f64 -> i64` (the rounding family).
    RealToInt,
    /// `(f64, f64) -> f64` (`pow`, `atan2`, `hypot`).
    RealRealToReal,
}

macro_rules! real_to_real {
    ($($symbol:ident / $leek:literal => $body:expr;)*) => {
        $(
            /// Shared `f64 -> f64` math builtin. `no_mangle` so the AOT
            /// (compile-to-executable) backend can link against it by name.
            #[allow(unsafe_code)]
            #[unsafe(no_mangle)]
            pub extern "C" fn $symbol(x: f64) -> f64 {
                let f: fn(f64) -> f64 = $body;
                f(x)
            }
        )*
    };
}

macro_rules! real_to_int {
    ($($symbol:ident / $leek:literal => $body:expr;)*) => {
        $(
            /// Shared `f64 -> i64` math builtin. `no_mangle` for AOT linking.
            #[allow(unsafe_code)]
            #[unsafe(no_mangle)]
            pub extern "C" fn $symbol(x: f64) -> i64 {
                let f: fn(f64) -> i64 = $body;
                f(x)
            }
        )*
    };
}

real_to_real! {
    leek_sqrt   / "sqrt"      => |x| x.sqrt();
    leek_cbrt   / "cbrt"      => |x| x.cbrt();
    leek_sin    / "sin"       => |x| x.sin();
    leek_cos    / "cos"       => |x| x.cos();
    leek_tan    / "tan"       => |x| x.tan();
    leek_asin   / "asin"      => |x| x.asin();
    leek_acos   / "acos"      => |x| x.acos();
    leek_atan   / "atan"      => |x| x.atan();
    leek_sinh   / "sinh"      => |x| x.sinh();
    leek_cosh   / "cosh"      => |x| x.cosh();
    leek_tanh   / "tanh"      => |x| x.tanh();
    leek_exp    / "exp"       => |x| x.exp();
    leek_log    / "log"       => |x| x.ln();
    leek_log10  / "log10"     => |x| x.log10();
    leek_log2   / "log2"      => |x| x.log2();
    leek_to_degrees / "toDegrees" => |x| x.to_degrees();
    leek_to_radians / "toRadians" => |x| x.to_radians();
}

real_to_int! {
    leek_floor  / "floor"     => |x| crate::real_to_int(x.floor());
    leek_ceil   / "ceil"      => |x| crate::real_to_int(x.ceil());
    leek_round  / "round"     => |x| crate::real_to_int(x.round());
}

/// Shared `pow` (always real, like the `pow` builtin).
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn leek_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

/// Shared `atan2`.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn leek_atan2(a: f64, b: f64) -> f64 {
    a.atan2(b)
}

/// Shared `hypot`.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn leek_hypot(a: f64, b: f64) -> f64 {
    a.hypot(b)
}

/// Integer power for the `**` operator's all-integer path with a
/// non-negative exponent `< 64`. Matches the interpreter:
/// `a.checked_pow(b) ` saturating to `i64::MAX` on overflow. Callers must
/// guarantee `0 <= b < 64`.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn leek_ipow(a: i64, b: i64) -> i64 {
    a.checked_pow(u32::try_from(b).unwrap_or(u32::MAX)).unwrap_or(i64::MAX)
}

/// Address of [`leek_ipow`] for the native backend's `**` lowering (it's
/// not a named source builtin, so it's not in [`math_builtins`]).
pub fn ipow_addr() -> (&'static str, *const u8) {
    ("leek_ipow", leek_ipow as *const u8)
}

/// A scalar math builtin: its Leekscript name, the FFI symbol name, the
/// scalar signature, and the function address (for JIT registration).
#[derive(Clone, Copy)]
pub struct MathBuiltin {
    /// The name as written in Leekscript source (`"sqrt"`).
    pub leek_name: &'static str,
    /// A stable, collision-free symbol name (`"leek_sqrt"`) the native
    /// backend uses when declaring the imported function.
    pub symbol: &'static str,
    /// The scalar signature.
    pub sig: MathSig,
    /// The function's address, for `JITBuilder::symbol`.
    pub addr: *const u8,
}

/// The full table of shared scalar math builtins. Returned as a `Vec` (it
/// holds raw fn addresses, which aren't `Sync`, so it can't be a static).
pub fn math_builtins() -> Vec<MathBuiltin> {
    macro_rules! entry {
        ($symbol:ident, $leek:literal, $sig:expr) => {
            MathBuiltin {
                leek_name: $leek,
                symbol: stringify!($symbol),
                sig: $sig,
                addr: $symbol as *const u8,
            }
        };
    }
    vec![
        entry!(leek_sqrt, "sqrt", MathSig::RealToReal),
        entry!(leek_cbrt, "cbrt", MathSig::RealToReal),
        entry!(leek_sin, "sin", MathSig::RealToReal),
        entry!(leek_cos, "cos", MathSig::RealToReal),
        entry!(leek_tan, "tan", MathSig::RealToReal),
        entry!(leek_asin, "asin", MathSig::RealToReal),
        entry!(leek_acos, "acos", MathSig::RealToReal),
        entry!(leek_atan, "atan", MathSig::RealToReal),
        entry!(leek_sinh, "sinh", MathSig::RealToReal),
        entry!(leek_cosh, "cosh", MathSig::RealToReal),
        entry!(leek_tanh, "tanh", MathSig::RealToReal),
        entry!(leek_exp, "exp", MathSig::RealToReal),
        entry!(leek_log, "log", MathSig::RealToReal),
        entry!(leek_log10, "log10", MathSig::RealToReal),
        entry!(leek_log2, "log2", MathSig::RealToReal),
        entry!(leek_to_degrees, "toDegrees", MathSig::RealToReal),
        entry!(leek_to_radians, "toRadians", MathSig::RealToReal),
        entry!(leek_floor, "floor", MathSig::RealToInt),
        entry!(leek_ceil, "ceil", MathSig::RealToInt),
        entry!(leek_round, "round", MathSig::RealToInt),
        entry!(leek_pow, "pow", MathSig::RealRealToReal),
        entry!(leek_atan2, "atan2", MathSig::RealRealToReal),
        entry!(leek_hypot, "hypot", MathSig::RealRealToReal),
    ]
}

/// Look up the scalar signature of a math builtin by its Leekscript name.
pub fn math_sig(leek_name: &str) -> Option<MathSig> {
    math_builtins()
        .into_iter()
        .find(|b| b.leek_name == leek_name)
        .map(|b| b.sig)
}
