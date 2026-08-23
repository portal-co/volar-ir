//! Small, bounded RISC-V programs that exercise the generic
//! WASM-to-VAFFLE-to-IR pipeline without depending on Volar's compiler or
//! proof stack.

use portal_pc_waffle_frontend::{expand_func, FrontendOptions, Module as WModule};

pub mod interp;
pub mod wat_gen;

/// Parse a WASM binary into a fully expanded WAFFLE module, ready for
/// [`volar_vaffle_target::waffle_lower::lower_waffle_module`].
pub fn parse_and_expand(wasm_bytes: &[u8]) -> anyhow::Result<WModule<'_>> {
    let mut module = portal_pc_waffle_frontend::from_wasm_bytes(
        wasm_bytes,
        &FrontendOptions { debug: false },
    )?;
    let func_ids: Vec<_> = module.funcs.entries().map(|(id, _)| id).collect();
    for id in func_ids {
        expand_func(&mut module, id)?;
    }
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use volar_vaffle_target::{
        import_config::WaffleImportConfig, waffle_lower::lower_waffle_module, VaffleTarget,
    };

    #[test]
    fn bounded_riscv_wat_lowers_with_static_memory_initialization() {
        let wasm_bytes = wat::parse_str(wat_gen::test_program_wat()).expect("WAT should assemble");
        let module = parse_and_expand(&wasm_bytes).expect("WASM should parse and expand");
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(
            &module,
            &mut target,
            &WaffleImportConfig::default().with_memory_address_bits(5),
        );

        assert!(errors.is_empty(), "unexpected lowering errors: {errors:?}");
        assert_eq!(target.module.pre_init.len(), 2, "code and data memories are initialized");
        assert!(target.module.pre_init.iter().all(|segment| segment.offset == 0));
    }
}
