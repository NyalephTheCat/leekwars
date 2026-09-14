//! The runtime's own id types are opaque handles: it stores and compares them,
//! never interprets them. Two ids that differ must compare unequal wherever a
//! `Value` carries one, because that identity is all the runtime has to tell
//! one class (or one user function) from another — `x instanceof C`,
//! `f == g` and `.class` equality all ride on it.
//!
//! These used to be `leek_hir::DefId`, which forced `crates/core/leek-runtime`
//! to depend on a middle-layer crate (ARCH-09 / RT-10). `cargo xtask
//! check-layers` is the gate on the dependency; this pins the behaviour the
//! swap had to preserve.

use leek_runtime::{ClassId, FnId, Function, Instance, ObjectData, Value, value_instanceof};
use std::cell::RefCell;
use std::rc::Rc;

fn class_ref(id: u32, name: &str) -> Value {
    Value::ClassRef(ClassId(id), Rc::new(name.to_string()))
}

fn instance(id: u32, name: &str) -> Value {
    Value::Instance(Rc::new(RefCell::new(Instance {
        class: ClassId(id),
        class_name: name.to_string(),
        fields: ObjectData::new(),
    })))
}

#[test]
fn class_refs_compare_by_id_not_by_name() {
    assert!(class_ref(7, "A").loose_eq(&class_ref(7, "A")));
    // Same source name, different class — two files, two declarations.
    assert!(!class_ref(7, "A").loose_eq(&class_ref(8, "A")));
}

#[test]
fn user_function_values_compare_by_id() {
    let f = Value::Function(Function::User(FnId(3)));
    assert!(f.loose_eq(&Value::Function(Function::User(FnId(3)))));
    assert!(!f.loose_eq(&Value::Function(Function::User(FnId(4)))));
}

#[test]
fn instanceof_matches_the_instances_class_id() {
    let a = class_ref(7, "A");
    let b = class_ref(8, "B");
    assert!(value_instanceof(&instance(7, "A"), &a));
    assert!(!value_instanceof(&instance(7, "A"), &b));
}

#[test]
fn the_class_meta_property_carries_the_instances_id() {
    let Value::ClassRef(id, name) = leek_runtime::class_of(&instance(7, "A")) else {
        panic!("`.class` of an instance is a class reference");
    };
    assert_eq!(id, ClassId(7));
    assert_eq!(name.as_str(), "A");
}
