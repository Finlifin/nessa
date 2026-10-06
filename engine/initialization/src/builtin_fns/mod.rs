//! Builtin function implementations registered at engine init.

mod convert;
mod io;
mod list;
mod math;
mod process;
mod string;

use interpreter::Vm;
use runtime::{catalog_register_intrinsic_types, ids};

/// Register all builtin functions and type names into the VM / global catalog.
pub fn register_all(vm: &mut Vm) {
    catalog_register_intrinsic_types(&vm.type_pool);

    vm.register_builtin(ids::PRINT, io::print);
    vm.register_builtin(ids::PRINTLN, io::println);

    vm.register_builtin(ids::TYPE_OF, convert::type_of);
    vm.register_builtin(ids::TO_I64, convert::to_i64);
    vm.register_builtin(ids::TO_F64, convert::to_f64);
    vm.register_builtin(ids::TO_STRING, convert::to_string);

    vm.register_builtin(ids::ABS, math::abs);
    vm.register_builtin(ids::SIN, math::sin);
    vm.register_builtin(ids::COS, math::cos);
    vm.register_builtin(ids::SQRT, math::sqrt);
    vm.register_builtin(ids::FLOOR, math::floor);
    vm.register_builtin(ids::CEIL, math::ceil);
    vm.register_builtin(ids::ROUND, math::round);
    vm.register_builtin(ids::POW, math::pow);
    vm.register_builtin(ids::LOG, math::log);

    vm.register_builtin(ids::STR_LEN, string::str_len);
    vm.register_builtin(ids::STR_CONCAT, string::str_concat);

    vm.register_builtin(ids::EXIT, process::exit);
    vm.register_builtin(ids::PANIC, process::panic);

    vm.register_builtin(ids::LIST_INIT, list::list_init);
}
