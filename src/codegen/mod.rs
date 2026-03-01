
use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, DataDescription, DataId, FuncId};
use cranelift::codegen::ir::UserFuncName;
use std::collections::HashMap;
use crate::ast::{Program, Statement, Expression, BinaryOp, Type, Function};

pub mod aot;

/// Selección de nivel de optimización de Cranelift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CraneliftOptLevel {
    None,
    Speed,
    SpeedAndSize,
}

/// Información de una función definida por el usuario, necesaria en los sitios de llamada
pub(crate) struct UserFuncInfo {
    pub(crate) func_id: FuncId,
    pub(crate) params: Vec<(String, Type, bool)>, // (nombre, tipo, por_referencia)
    pub(crate) return_type: Option<Type>,
    pub(crate) has_return: bool,
}

pub struct CodeGenerator {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: JITModule,
    string_literals: HashMap<String, DataId>, // Mapa de literales de cadena
}

impl CodeGenerator {
    pub fn new() -> Self {
        Self::with_opt_level(CraneliftOptLevel::None)
    }

    pub fn with_opt_level(opt: CraneliftOptLevel) -> Self {
        // Configurar flags de Cranelift según nivel de optimización
        let mut flag_builder = settings::builder();
        match opt {
            CraneliftOptLevel::None => {
                flag_builder.set("opt_level", "none").unwrap();
            }
            CraneliftOptLevel::Speed => {
                flag_builder.set("opt_level", "speed").unwrap();
            }
            CraneliftOptLevel::SpeedAndSize => {
                flag_builder.set("opt_level", "speed_and_size").unwrap();
            }
        }
        // Habilitar código independiente de posición para JIT
        flag_builder.set("is_pic", "true").unwrap();

        let isa_builder = cranelift_native::builder().unwrap_or_else(|msg| {
            panic!("ISA de la máquina host no soportada: {}", msg);
        });
        let isa = isa_builder.finish(settings::Flags::new(flag_builder)).unwrap();

        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        // Registrar funciones de E/S
        builder.symbol("print_int", print_int as *const u8);
        builder.symbol("print_real", print_real as *const u8);
        builder.symbol("print_str", print_str as *const u8);
        builder.symbol("print_newline", print_newline as *const u8);
        builder.symbol("read_int", read_int as *const u8);
        builder.symbol("read_real", read_real as *const u8);
        // Operador potencia
        builder.symbol("builtin_power", builtin_power as *const u8);
        // Funciones matemáticas nativas (f64 -> f64)
        builder.symbol("builtin_rc", builtin_rc as *const u8);
        builder.symbol("builtin_abs", builtin_abs as *const u8);
        builder.symbol("builtin_ln", builtin_ln as *const u8);
        builder.symbol("builtin_exp", builtin_exp as *const u8);
        builder.symbol("builtin_sen", builtin_sen as *const u8);
        builder.symbol("builtin_cos", builtin_cos as *const u8);
        builder.symbol("builtin_tan", builtin_tan as *const u8);
        builder.symbol("builtin_asen", builtin_asen as *const u8);
        builder.symbol("builtin_acos", builtin_acos as *const u8);
        builder.symbol("builtin_atan", builtin_atan as *const u8);
        // Funciones matemáticas nativas (f64 -> i64)
        builder.symbol("builtin_trunc", builtin_trunc as *const u8);
        builder.symbol("builtin_redon", builtin_redon as *const u8);
        // Aleatorio
        builder.symbol("builtin_azar", builtin_azar as *const u8);
        builder.symbol("builtin_aleatorio", builtin_aleatorio as *const u8);
        // Funciones de cadenas nativas
        builder.symbol("builtin_longitud", builtin_longitud as *const u8);
        builder.symbol("builtin_mayusculas", builtin_mayusculas as *const u8);
        builder.symbol("builtin_minusculas", builtin_minusculas as *const u8);
        builder.symbol("builtin_subcadena", builtin_subcadena as *const u8);
        builder.symbol("builtin_concatenar", builtin_concatenar as *const u8);
        // Conversión
        builder.symbol("builtin_convertiranumero", builtin_convertiranumero as *const u8);
        builder.symbol("builtin_convertiratexto", builtin_convertiratexto as *const u8);
        builder.symbol("builtin_int_to_str", builtin_int_to_str as *const u8);
        // Tiempo
        builder.symbol("builtin_horaactual", builtin_horaactual as *const u8);
        builder.symbol("builtin_fechaactual", builtin_fechaactual as *const u8);
        // Arreglos
        builder.symbol("builtin_alloc_array", builtin_alloc_array as *const u8);
        // Pantalla / salida
        builder.symbol("builtin_clear_screen", builtin_clear_screen as *const u8);
        builder.symbol("flush_stdout", flush_stdout as *const u8);
        builder.symbol("builtin_sleep_secs", builtin_sleep_secs as *const u8);
        builder.symbol("builtin_sleep_millis", builtin_sleep_millis as *const u8);
        builder.symbol("builtin_wait_key", builtin_wait_key as *const u8);
        
        let module = JITModule::new(builder);
        
        Self {
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            string_literals: HashMap::new(),
        }
    }

    pub fn compile(&mut self, program: &Program) -> Result<*const u8, String> {
        // Extraer dimensiones constantes de arreglos de todo el programa
        let global_array_dims = extract_constant_array_dims(program);

        // 1. Declarar todas las funciones de usuario y recopilar info
        let mut user_functions: HashMap<String, UserFuncInfo> = HashMap::new();
        for func in &program.functions {
            let mut sig = self.module.make_signature();
            for (_, ty, _) in &func.params {
                let cl_ty = match ty { Type::Real => types::F64, _ => types::I64 };
                sig.params.push(AbiParam::new(cl_ty));
            }
            if func.return_var.is_some() {
                let ret_ty = match &func.return_type {
                    Some(Type::Real) => types::F64, _ => types::I64,
                };
                sig.returns.push(AbiParam::new(ret_ty));
            }
            for (_, ty, by_ref) in &func.params {
                if *by_ref {
                    let cl_ty = match ty { Type::Real => types::F64, _ => types::I64 };
                    sig.returns.push(AbiParam::new(cl_ty));
                }
            }
            let func_id = self.module.declare_function(
                &func.name, Linkage::Local, &sig,
            ).map_err(|e| e.to_string())?;
            user_functions.insert(func.name.to_lowercase(), UserFuncInfo {
                func_id,
                params: func.params.clone(),
                return_type: func.return_type.clone(),
                has_return: func.return_var.is_some(),
            });
        }

        // 2. Compilar cada función de usuario
        for func in &program.functions {
            self.compile_user_function(func, &user_functions, &global_array_dims)?;
        }

        // 3. Compilar main (proceso principal)
        let mut sig_main = self.module.make_signature();
        sig_main.returns.push(AbiParam::new(types::I32));

        let main_id = self.module.declare_function(
            &program.name,
            Linkage::Export,
            &sig_main,
        ).map_err(|e| e.to_string())?;

        self.ctx.func.signature = sig_main;
        self.ctx.func.name = UserFuncName::user(0, main_id.as_u32()); 

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let mut trans = FunctionTranslator {
                builder,
                variables: HashMap::new(),
                variable_types: HashMap::new(),
                module: &mut self.module,
                string_literals: &mut self.string_literals,
                user_functions: &user_functions,
                array_dims: HashMap::new(),
                array_elem_types: HashMap::new(),
                global_array_dims: &global_array_dims,
            };

            for stmt in &program.main_body {
                 trans.translate_stmt(stmt);
            }

            let zero = trans.builder.ins().iconst(types::I32, 0);
            trans.builder.ins().return_(&[zero]);
            
            trans.builder.finalize();
        }

        self.module.define_function(main_id, &mut self.ctx).map_err(|e| e.to_string())?;
        
        self.module.clear_context(&mut self.ctx);
        self.module.finalize_definitions().unwrap();
        
        let code = self.module.get_finalized_function(main_id);
        Ok(code)
    }

    fn compile_user_function(&mut self, func: &Function, user_functions: &HashMap<String, UserFuncInfo>, global_array_dims: &HashMap<String, Vec<i64>>) -> Result<(), String> {
        let info = user_functions.get(&func.name.to_lowercase()).unwrap();

        // Reconstruir firma
        let mut sig = self.module.make_signature();
        for (_, ty, _) in &func.params {
            let cl_ty = match ty { Type::Real => types::F64, _ => types::I64 };
            sig.params.push(AbiParam::new(cl_ty));
        }
        if func.return_var.is_some() {
            let ret_ty = match &func.return_type {
                Some(Type::Real) => types::F64, _ => types::I64,
            };
            sig.returns.push(AbiParam::new(ret_ty));
        }
        for (_, ty, by_ref) in &func.params {
            if *by_ref {
                let cl_ty = match ty { Type::Real => types::F64, _ => types::I64 };
                sig.returns.push(AbiParam::new(cl_ty));
            }
        }

        self.ctx.func.signature = sig;
        self.ctx.func.name = UserFuncName::user(0, info.func_id.as_u32());

        {
            let mut builder = FunctionBuilder::new(&mut self.ctx.func, &mut self.builder_context);
            let entry_block = builder.create_block();
            builder.append_block_params_for_function_params(entry_block);
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);

            let mut variables: HashMap<String, Variable> = HashMap::new();
            let mut variable_types: HashMap<String, Type> = HashMap::new();
            let block_params = builder.block_params(entry_block).to_vec();

            // Declarar variables de parámetros
            for (i, (name, ty, _)) in func.params.iter().enumerate() {
                let var = Variable::new(variables.len());
                let cl_ty = match ty { Type::Real => types::F64, _ => types::I64 };
                builder.declare_var(var, cl_ty);
                builder.def_var(var, block_params[i]);
                variables.insert(name.clone(), var);
                variable_types.insert(name.clone(), ty.clone());
            }

            // Declarar variable de retorno si existe
            if let Some(ref ret_var) = func.return_var {
                let var = Variable::new(variables.len());
                let cl_ty = match &func.return_type {
                    Some(Type::Real) => types::F64, _ => types::I64,
                };
                builder.declare_var(var, cl_ty);
                let init = match &func.return_type {
                    Some(Type::Real) => builder.ins().f64const(0.0),
                    _ => builder.ins().iconst(types::I64, 0),
                };
                builder.def_var(var, init);
                variables.insert(ret_var.clone(), var);
                variable_types.insert(ret_var.clone(), func.return_type.clone().unwrap_or(Type::Integer));
            }

            let mut trans = FunctionTranslator {
                builder,
                variables,
                variable_types,
                module: &mut self.module,
                string_literals: &mut self.string_literals,
                user_functions,
                array_dims: HashMap::new(),
                array_elem_types: HashMap::new(),
                global_array_dims,
            };

            for stmt in &func.body {
                trans.translate_stmt(stmt);
            }

            // Construir valores de retorno: primero return_var, luego parámetros por referencia
            let mut return_vals = Vec::new();
            if let Some(ref ret_var) = func.return_var {
                if let Some(var) = trans.variables.get(ret_var) {
                    let val = trans.builder.use_var(*var);
                    return_vals.push(val);
                }
            }
            for (name, _, by_ref) in &func.params {
                if *by_ref {
                    if let Some(var) = trans.variables.get(name) {
                        let val = trans.builder.use_var(*var);
                        return_vals.push(val);
                    }
                }
            }
            trans.builder.ins().return_(&return_vals);
            trans.builder.finalize();
        }

        self.module.define_function(info.func_id, &mut self.ctx).map_err(|e| e.to_string())?;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }
}

pub(crate) struct FunctionTranslator<'a, M: Module> {
    pub(crate) builder: FunctionBuilder<'a>,
    pub(crate) variables: HashMap<String, Variable>, 
    pub(crate) variable_types: HashMap<String, Type>,
    pub(crate) module: &'a mut M,
    pub(crate) string_literals: &'a mut HashMap<String, DataId>,
    pub(crate) user_functions: &'a HashMap<String, UserFuncInfo>,
    pub(crate) array_dims: HashMap<String, Vec<Value>>,
    pub(crate) array_elem_types: HashMap<String, Type>,
    pub(crate) global_array_dims: &'a HashMap<String, Vec<i64>>,
}

/// Describe la firma de una función nativa para codegen
struct BuiltinSig {
    runtime_name: &'static str,
    params: &'static [types::Type],
    ret: types::Type,
    ret_ast: Type,
}

fn lookup_builtin(name: &str) -> Option<BuiltinSig> {
    match name {
        // Matemáticas: f64 -> f64
        "rc" | "raiz" => Some(BuiltinSig { runtime_name: "builtin_rc", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "abs"         => Some(BuiltinSig { runtime_name: "builtin_abs", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "ln"          => Some(BuiltinSig { runtime_name: "builtin_ln", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "exp"         => Some(BuiltinSig { runtime_name: "builtin_exp", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "sen"         => Some(BuiltinSig { runtime_name: "builtin_sen", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "cos"         => Some(BuiltinSig { runtime_name: "builtin_cos", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "tan"         => Some(BuiltinSig { runtime_name: "builtin_tan", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "asen"        => Some(BuiltinSig { runtime_name: "builtin_asen", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "acos"        => Some(BuiltinSig { runtime_name: "builtin_acos", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        "atan"        => Some(BuiltinSig { runtime_name: "builtin_atan", params: &[types::F64], ret: types::F64, ret_ast: Type::Real }),
        // Matemáticas: f64 -> i64
        "trunc"       => Some(BuiltinSig { runtime_name: "builtin_trunc", params: &[types::F64], ret: types::I64, ret_ast: Type::Integer }),
        "redon"       => Some(BuiltinSig { runtime_name: "builtin_redon", params: &[types::F64], ret: types::I64, ret_ast: Type::Integer }),
        // Aleatorio
        "azar"        => Some(BuiltinSig { runtime_name: "builtin_azar", params: &[types::I64], ret: types::I64, ret_ast: Type::Integer }),
        "aleatorio"   => Some(BuiltinSig { runtime_name: "builtin_aleatorio", params: &[types::I64, types::I64], ret: types::I64, ret_ast: Type::Integer }),
        // Cadena -> Entero
        "longitud"    => Some(BuiltinSig { runtime_name: "builtin_longitud", params: &[types::I64], ret: types::I64, ret_ast: Type::Integer }),
        // Cadena -> Cadena
        "mayusculas"  => Some(BuiltinSig { runtime_name: "builtin_mayusculas", params: &[types::I64], ret: types::I64, ret_ast: Type::String }),
        "minusculas"  => Some(BuiltinSig { runtime_name: "builtin_minusculas", params: &[types::I64], ret: types::I64, ret_ast: Type::String }),
        // Manipulación de cadenas
        "subcadena"   => Some(BuiltinSig { runtime_name: "builtin_subcadena", params: &[types::I64, types::I64, types::I64], ret: types::I64, ret_ast: Type::String }),
        "concatenar"  => Some(BuiltinSig { runtime_name: "builtin_concatenar", params: &[types::I64, types::I64], ret: types::I64, ret_ast: Type::String }),
        // Conversión
        "convertiranumero" => Some(BuiltinSig { runtime_name: "builtin_convertiranumero", params: &[types::I64], ret: types::F64, ret_ast: Type::Real }),
        "convertiratexto"  => Some(BuiltinSig { runtime_name: "builtin_convertiratexto", params: &[types::F64], ret: types::I64, ret_ast: Type::String }),
        // Tiempo
        "horaactual"  => Some(BuiltinSig { runtime_name: "builtin_horaactual", params: &[], ret: types::I64, ret_ast: Type::Integer }),
        "fechaactual" => Some(BuiltinSig { runtime_name: "builtin_fechaactual", params: &[], ret: types::I64, ret_ast: Type::Integer }),
        _ => None,
    }
}

/// Extrae dimensiones constantes de arreglos de todas las sentencias Dimension del programa
pub(crate) fn extract_constant_array_dims(program: &Program) -> HashMap<String, Vec<i64>> {
    let mut dims = HashMap::new();
    fn scan_stmts(stmts: &[Statement], dims: &mut HashMap<String, Vec<i64>>) {
        for stmt in stmts {
            if let Statement::Dimension { name, sizes } = stmt {
                let const_sizes: Vec<i64> = sizes.iter().filter_map(|e| {
                    if let Expression::Literal(crate::ast::Literal::Integer(n)) = e {
                        Some(*n)
                    } else {
                        None
                    }
                }).collect();
                if const_sizes.len() == sizes.len() {
                    dims.insert(name.to_lowercase(), const_sizes);
                }
            }
            match stmt {
                Statement::If { then_branch, else_branch, .. } => {
                    scan_stmts(then_branch, dims);
                    if let Some(eb) = else_branch { scan_stmts(eb, dims); }
                }
                Statement::While { body, .. } | Statement::Repeat { body, .. } | Statement::For { body, .. } => {
                    scan_stmts(body, dims);
                }
                _ => {}
            }
        }
    }
    scan_stmts(&program.main_body, &mut dims);
    for func in &program.functions {
        scan_stmts(&func.body, &mut dims);
    }
    dims
}

impl<'a, M: Module> FunctionTranslator<'a, M> {
    fn ensure_variable(&mut self, name: &str, ty: Type) -> Variable {
        if let Some(var) = self.variables.get(name) {
            return *var;
        }

        let variable = Variable::new(self.variables.len());
        let cl_type = match ty {
            Type::Real => types::F64,
            Type::String => types::I64, // Puntero como I64
            _ => types::I64,
        };
        self.builder.declare_var(variable, cl_type);
        self.variables.insert(name.to_string(), variable);
        self.variable_types.insert(name.to_string(), ty);
        variable
    }

    /// Convierte un valor a puntero de cadena (i64) para concatenación
    fn coerce_to_string(&mut self, val: Value, ty: &Type) -> Value {
        if *ty == Type::String {
            return val;
        }
        if *ty == Type::Real {
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(types::F64));
            sig.returns.push(AbiParam::new(types::I64));
            let callee = self.module.declare_function("builtin_convertiratexto", Linkage::Import, &sig).unwrap();
            let local = self.module.declare_func_in_func(callee, self.builder.func);
            let call = self.builder.ins().call(local, &[val]);
            self.builder.inst_results(call)[0]
        } else {
            let mut sig = self.module.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            sig.returns.push(AbiParam::new(types::I64));
            let callee = self.module.declare_function("builtin_int_to_str", Linkage::Import, &sig).unwrap();
            let local = self.module.declare_func_in_func(callee, self.builder.func);
            let call = self.builder.ins().call(local, &[val]);
            self.builder.inst_results(call)[0]
        }
    }

    fn compute_flat_index(&mut self, array: &str, indices: &[Expression]) -> Value {
        let one = self.builder.ins().iconst(types::I64, 1);
        if indices.len() == 1 {
            let (idx, _) = self.translate_expr(&indices[0]);
            self.builder.ins().isub(idx, one)
        } else {
            // Intentar dims locales, luego globales
            let dims: Vec<Value> = if let Some(d) = self.array_dims.get(array) {
                d.clone()
            } else if let Some(sizes) = self.global_array_dims.get(&array.to_lowercase()) {
                sizes.iter().map(|&s| self.builder.ins().iconst(types::I64, s)).collect()
            } else {
                vec![]
            };
            let mut flat = self.builder.ins().iconst(types::I64, 0);
            for (k, idx_expr) in indices.iter().enumerate() {
                let (idx, _) = self.translate_expr(idx_expr);
                let idx_zero = self.builder.ins().isub(idx, one);
                let mut stride = self.builder.ins().iconst(types::I64, 1);
                for dim_val in dims.iter().skip(k + 1) {
                    stride = self.builder.ins().imul(stride, *dim_val);
                }
                let contrib = self.builder.ins().imul(idx_zero, stride);
                flat = self.builder.ins().iadd(flat, contrib);
            }
            flat
        }
    }

    fn translate_stmt(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Define { vars, ty } => {
                let cl_type = match ty {
                    Type::Integer => types::I64,
                    Type::Real => types::F64,
                    Type::Boolean => types::I64,
                    Type::String => types::I64, // Puntero como I64
                    _ => types::I64,
                };
                for (_i, var) in vars.iter().enumerate() {
                    if !self.variables.contains_key(var) {
                        let variable = Variable::new(self.variables.len());
                        self.builder.declare_var(variable, cl_type);
                        self.variables.insert(var.clone(), variable);
                        self.variable_types.insert(var.clone(), ty.clone());
                    }
                }
            }
             Statement::Assign { target, value } => {
                  let (val, val_ty) = self.translate_expr(value);
                  let var = self.ensure_variable(target, val_ty);
                  // Coercer valor para coincidir con el tipo declarado de la variable
                  let var_val = self.builder.use_var(var);
                  let var_cl_ty = self.builder.func.dfg.value_type(var_val);
                  let val_cl_ty = self.builder.func.dfg.value_type(val);
                  let val = if var_cl_ty == types::F64 && val_cl_ty == types::I64 {
                      self.builder.ins().fcvt_from_sint(types::F64, val)
                  } else if var_cl_ty == types::I64 && val_cl_ty == types::F64 {
                      self.builder.ins().fcvt_to_sint_sat(types::I64, val)
                  } else {
                      val
                  };
                  self.builder.def_var(var, val);
             }
             Statement::Dimension { name, sizes } => {
                 // Evaluar todos los tamaños de dimensión y calcular total
                 let mut dim_vals = Vec::new();
                 let mut total = {
                     let (first, _) = self.translate_expr(&sizes[0]);
                     dim_vals.push(first);
                     first
                 };
                 for size_expr in &sizes[1..] {
                     let (sz, _) = self.translate_expr(size_expr);
                     dim_vals.push(sz);
                     total = self.builder.ins().imul(total, sz);
                 }
                 self.array_dims.insert(name.clone(), dim_vals);
                 // Llamar al asignador en tiempo de ejecución: retorna puntero como i64
                 let mut sig = self.module.make_signature();
                 sig.params.push(AbiParam::new(types::I64));
                 sig.returns.push(AbiParam::new(types::I64));
                 let callee = self.module.declare_function("builtin_alloc_array", Linkage::Import, &sig).unwrap();
                 let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                 let call = self.builder.ins().call(local_callee, &[total]);
                 let ptr = self.builder.inst_results(call)[0];
                 // Almacenar puntero en variable
                 let var = self.ensure_variable(name, Type::Integer);
                 self.builder.def_var(var, ptr);
             }
             Statement::IndexAssign { array, indices, value } => {
                 // Obtener puntero base del arreglo
                 let base_ptr = if let Some(v) = self.variables.get(array) {
                     self.builder.use_var(*v)
                 } else {
                     self.builder.ins().iconst(types::I64, 0)
                 };
                 // Calcular índice plano usando info de dimensiones
                 let flat_idx = self.compute_flat_index(array, indices);
                 let eight = self.builder.ins().iconst(types::I64, 8);
                 let offset = self.builder.ins().imul(flat_idx, eight);
                 let addr = self.builder.ins().iadd(base_ptr, offset);
                 // Traducir valor
                 let (val, val_ty) = self.translate_expr(value);
                 // Rastrear tipo de elemento
                 self.array_elem_types.entry(array.clone()).or_insert(val_ty.clone());
                 // Coercer real a bits i64 para almacenamiento
                 let val = if val_ty == Type::Real {
                     self.builder.ins().bitcast(types::I64, MemFlags::new(), val)
                 } else {
                     val
                 };
                 // Almacenar
                 self.builder.ins().store(MemFlags::new(), val, addr, 0);
             }
             Statement::Read(targets) => {
                 for target in targets {
                     match target {
                         Expression::Variable(var_name) => {
                             let var = if let Some(v) = self.variables.get(var_name) {
                                 *v
                             } else {
                                 self.ensure_variable(var_name, Type::Integer)
                             };

                             let val_stub = self.builder.use_var(var);
                             let ty = self.builder.func.dfg.value_type(val_stub);
                             
                             let func_name = if ty == types::F64 { "read_real" } else { "read_int" };
                             let ret_ty = if ty == types::F64 { types::F64 } else { types::I64 };

                             let mut sig = self.module.make_signature();
                             sig.returns.push(AbiParam::new(ret_ty));
                             
                             let callee = self.module.declare_function(func_name, Linkage::Import, &sig).unwrap();
                             let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                             let call = self.builder.ins().call(local_callee, &[]);
                             let result = self.builder.inst_results(call)[0];
                             
                             self.builder.def_var(var, result);
                         }
                         Expression::Index { array, indices } => {
                             // Leer en elemento de arreglo: Leer datos[i]
                             let mut sig = self.module.make_signature();
                             sig.returns.push(AbiParam::new(types::I64));
                             let callee = self.module.declare_function("read_int", Linkage::Import, &sig).unwrap();
                             let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                             let call = self.builder.ins().call(local_callee, &[]);
                             let result = self.builder.inst_results(call)[0];

                             let base_ptr = if let Some(v) = self.variables.get(array) {
                                 self.builder.use_var(*v)
                             } else {
                                 self.builder.ins().iconst(types::I64, 0)
                             };
                             let flat_idx = self.compute_flat_index(array, indices);
                             let eight = self.builder.ins().iconst(types::I64, 8);
                             let offset = self.builder.ins().imul(flat_idx, eight);
                             let addr = self.builder.ins().iadd(base_ptr, offset);
                             self.builder.ins().store(MemFlags::new(), result, addr, 0);
                         }
                         _ => {}
                     }
                 }
             }
            Statement::Write(exprs, newline) => {
                 for expr in exprs {
                     let (val, ty) = self.translate_expr(expr);
                     let func_name = match ty {
                        Type::String => "print_str",
                        Type::Real => "print_real",
                        _ => "print_int"
                     };

                     let mut sig = self.module.make_signature();
                     let arg_ty = if ty == Type::Real { types::F64 } else { types::I64 };
                     sig.params.push(AbiParam::new(arg_ty));
                     sig.returns.push(AbiParam::new(types::I32));
                     
                     let callee = self.module.declare_function(func_name, Linkage::Import, &sig).unwrap();
                     let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                     self.builder.ins().call(local_callee, &[val]);
                 }
                 if *newline {
                     let mut sig_nl = self.module.make_signature();
                     sig_nl.returns.push(AbiParam::new(types::I32));
                     let callee_nl = self.module.declare_function("print_newline", Linkage::Import, &sig_nl).unwrap();
                     let local_nl = self.module.declare_func_in_func(callee_nl, self.builder.func);
                     self.builder.ins().call(local_nl, &[]);
                 } else {
                     let mut sig_fl = self.module.make_signature();
                     sig_fl.returns.push(AbiParam::new(types::I32));
                     let callee_fl = self.module.declare_function("flush_stdout", Linkage::Import, &sig_fl).unwrap();
                     let local_fl = self.module.declare_func_in_func(callee_fl, self.builder.func);
                     self.builder.ins().call(local_fl, &[]);
                 }
            }
            Statement::If { condition, then_branch, else_branch } => {
                let (cond_val, _) = self.translate_expr(condition);
                
                let then_block = self.builder.create_block();
                let else_block = self.builder.create_block();
                let merge_block = self.builder.create_block();

                self.builder.ins().brif(cond_val, then_block, &[], else_block, &[]);

                self.builder.switch_to_block(then_block);
                self.builder.seal_block(then_block);
                for stmt in then_branch {
                    self.translate_stmt(stmt);
                }
                self.builder.ins().jump(merge_block, &[]);

                self.builder.switch_to_block(else_block);
                self.builder.seal_block(else_block);
                if let Some(else_stmts) = else_branch {
                    for stmt in else_stmts {
                        self.translate_stmt(stmt);
                    }
                }
                self.builder.ins().jump(merge_block, &[]);

                self.builder.switch_to_block(merge_block);
                self.builder.seal_block(merge_block);
            }
            Statement::While { condition, body } => {
                let header_block = self.builder.create_block();
                let body_block = self.builder.create_block();
                let exit_block = self.builder.create_block();

                self.builder.ins().jump(header_block, &[]);

                self.builder.switch_to_block(header_block);
                let (cond_val, _) = self.translate_expr(condition);
                self.builder.ins().brif(cond_val, body_block, &[], exit_block, &[]);

                self.builder.switch_to_block(body_block);
                self.builder.seal_block(body_block);
                for stmt in body {
                    self.translate_stmt(stmt);
                }
                self.builder.ins().jump(header_block, &[]);

                self.builder.switch_to_block(exit_block);
                self.builder.seal_block(header_block); 
                self.builder.seal_block(exit_block);
            }
            Statement::Repeat { body, until } => {
                let body_block = self.builder.create_block();
                let exit_block = self.builder.create_block();

                self.builder.ins().jump(body_block, &[]);

                self.builder.switch_to_block(body_block);
                for stmt in body {
                    self.translate_stmt(stmt);
                }
                let (cond_val, _) = self.translate_expr(until);
                self.builder.ins().brif(cond_val, exit_block, &[], body_block, &[]);
                
                self.builder.seal_block(body_block); 
                
                self.builder.switch_to_block(exit_block);
                self.builder.seal_block(exit_block);
            }
            Statement::For { var, start, end, step, body } => {
                // Evaluar inicio y asignar a variable de bucle
                let (start_val, start_ty) = self.translate_expr(start);
                let loop_var = self.ensure_variable(var, start_ty.clone());
                self.builder.def_var(loop_var, start_val);

                // Evaluar paso (por defecto 1)
                let step_val = if let Some(step_expr) = step {
                    let (sv, _) = self.translate_expr(step_expr);
                    sv
                } else {
                    self.builder.ins().iconst(types::I64, 1)
                };

                let header_block = self.builder.create_block();
                let body_block = self.builder.create_block();
                let exit_block = self.builder.create_block();

                self.builder.ins().jump(header_block, &[]);

                // Cabecera: verificar condición
                self.builder.switch_to_block(header_block);
                let current = self.builder.use_var(loop_var);
                let (end_val, _) = self.translate_expr(end);

                // Determinar dirección: si paso > 0 entonces actual <= fin, sino actual >= fin
                let zero = self.builder.ins().iconst(types::I64, 0);
                let step_positive = self.builder.ins().icmp(IntCC::SignedGreaterThan, step_val, zero);
                let cond_le = self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, current, end_val);
                let cond_ge = self.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, current, end_val);
                let cond = self.builder.ins().select(step_positive, cond_le, cond_ge);

                self.builder.ins().brif(cond, body_block, &[], exit_block, &[]);

                // Cuerpo
                self.builder.switch_to_block(body_block);
                self.builder.seal_block(body_block);
                for stmt in body {
                    self.translate_stmt(stmt);
                }
                // Incrementar: var <- var + paso
                let current_after = self.builder.use_var(loop_var);
                let next = self.builder.ins().iadd(current_after, step_val);
                self.builder.def_var(loop_var, next);
                self.builder.ins().jump(header_block, &[]);

                self.builder.seal_block(header_block);
                self.builder.switch_to_block(exit_block);
                self.builder.seal_block(exit_block);
            }
            Statement::Call { function, args } => {
                // Delegar a translate_expr (maneja funciones nativas y de usuario + escritura por referencia)
                let call_expr = Expression::Call {
                    function: function.clone(),
                    args: args.clone(),
                };
                self.translate_expr(&call_expr);
            }
            Statement::ClearScreen => {
                let mut sig = self.module.make_signature();
                sig.returns.push(AbiParam::new(types::I32));
                let callee = self.module.declare_function("builtin_clear_screen", Linkage::Import, &sig).unwrap();
                let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                self.builder.ins().call(local_callee, &[]);
            }
            Statement::Wait { duration, milliseconds } => {
                let (dur_val, _) = self.translate_expr(duration);
                let func_name = if *milliseconds { "builtin_sleep_millis" } else { "builtin_sleep_secs" };
                let mut sig = self.module.make_signature();
                sig.params.push(AbiParam::new(types::I64));
                sig.returns.push(AbiParam::new(types::I32));
                let callee = self.module.declare_function(func_name, Linkage::Import, &sig).unwrap();
                let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                self.builder.ins().call(local_callee, &[dur_val]);
            }
            Statement::WaitKey => {
                let mut sig = self.module.make_signature();
                sig.returns.push(AbiParam::new(types::I32));
                let callee = self.module.declare_function("builtin_wait_key", Linkage::Import, &sig).unwrap();
                let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                self.builder.ins().call(local_callee, &[]);
            }
            _ => {}
        }
    }

    fn translate_expr(&mut self, expr: &Expression) -> (Value, Type) {
        match expr {
            Expression::Literal(lit) => match lit {
                crate::ast::Literal::Integer(i) => (self.builder.ins().iconst(types::I64, *i), Type::Integer),
                crate::ast::Literal::Real(f) => (self.builder.ins().f64const(*f), Type::Real),
                crate::ast::Literal::Boolean(b) => (self.builder.ins().iconst(types::I64, if *b { 1 } else { 0 }), Type::Boolean),
                crate::ast::Literal::String(s) => {
                    if let Some(data_id) = self.string_literals.get(s) {
                        let local_id = self.module.declare_data_in_func(*data_id, self.builder.func);
                        let val = self.builder.ins().global_value(types::I64, local_id);
                        return (val, Type::String);
                    }

                    let content = &s[1..s.len()-1];
                    let mut bytes = content.as_bytes().to_vec();
                    bytes.push(0);
                    
                    let data_id = self.module.declare_anonymous_data(true, false).unwrap();
                    let mut data_ctx = DataDescription::new();
                    data_ctx.define(bytes.into_boxed_slice());
                    self.module.define_data(data_id, &data_ctx).unwrap();
                    
                    self.string_literals.insert(s.clone(), data_id);

                    let local_id = self.module.declare_data_in_func(data_id, self.builder.func);
                    let val = self.builder.ins().global_value(types::I64, local_id);
                    (val, Type::String)
                },
            },
            Expression::Variable(name) => {
                if let Some(var) = self.variables.get(name) {
                    let val = self.builder.use_var(*var);
                    // Usar tipo rastreado si existe, sino inferir de Cranelift
                    let ast_ty = self.variable_types.get(name).cloned().unwrap_or_else(|| {
                        let ty = self.builder.func.dfg.value_type(val);
                        if ty == types::F64 { Type::Real } else { Type::Integer }
                    });
                    (val, ast_ty)
                } else {
                    (self.builder.ins().iconst(types::I64, 0), Type::Integer)
                }
            }
            Expression::Binary { left, op, right } => {
                 let (lhs, lhs_ty) = self.translate_expr(left);
                 let (rhs, rhs_ty) = self.translate_expr(right);

                 // Concatenación de cadenas con +
                 if matches!(op, BinaryOp::Add) && (lhs_ty == Type::String || rhs_ty == Type::String) {
                     let lhs_s = self.coerce_to_string(lhs, &lhs_ty);
                     let rhs_s = self.coerce_to_string(rhs, &rhs_ty);
                     let mut sig = self.module.make_signature();
                     sig.params.push(AbiParam::new(types::I64));
                     sig.params.push(AbiParam::new(types::I64));
                     sig.returns.push(AbiParam::new(types::I64));
                     let callee = self.module.declare_function("builtin_concatenar", Linkage::Import, &sig).unwrap();
                     let local = self.module.declare_func_in_func(callee, self.builder.func);
                     let call = self.builder.ins().call(local, &[lhs_s, rhs_s]);
                     let result = self.builder.inst_results(call)[0];
                     return (result, Type::String);
                 }
                 
                 // En PSeInt, / y ^ siempre producen Real, incluso con operandos enteros
                 let force_float = matches!(op, BinaryOp::Div | BinaryOp::Power);
                 let is_float = lhs_ty == Type::Real || rhs_ty == Type::Real || force_float;

                 let lhs = if lhs_ty == Type::Integer && is_float {
                     self.builder.ins().fcvt_from_sint(types::F64, lhs)
                 } else { lhs };
                 
                 let rhs = if rhs_ty == Type::Integer && is_float {
                     self.builder.ins().fcvt_from_sint(types::F64, rhs)
                 } else { rhs };

                 if is_float {
                     let val = match op {
                         BinaryOp::Add => self.builder.ins().fadd(lhs, rhs),
                         BinaryOp::Sub => self.builder.ins().fsub(lhs, rhs),
                         BinaryOp::Mul => self.builder.ins().fmul(lhs, rhs),
                         BinaryOp::Div => self.builder.ins().fdiv(lhs, rhs),
                         BinaryOp::Power => {
                             let mut sig = self.module.make_signature();
                             sig.params.push(AbiParam::new(types::F64));
                             sig.params.push(AbiParam::new(types::F64));
                             sig.returns.push(AbiParam::new(types::F64));
                             let callee = self.module.declare_function("builtin_power", Linkage::Import, &sig).unwrap();
                             let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                             let call = self.builder.ins().call(local_callee, &[lhs, rhs]);
                             self.builder.inst_results(call)[0]
                         }
                         BinaryOp::Eq => self.builder.ins().fcmp(FloatCC::Equal, lhs, rhs),
                         BinaryOp::Ne => self.builder.ins().fcmp(FloatCC::NotEqual, lhs, rhs),
                         BinaryOp::Lt => self.builder.ins().fcmp(FloatCC::LessThan, lhs, rhs),
                         BinaryOp::Le => self.builder.ins().fcmp(FloatCC::LessThanOrEqual, lhs, rhs),
                         BinaryOp::Gt => self.builder.ins().fcmp(FloatCC::GreaterThan, lhs, rhs),
                         BinaryOp::Ge => self.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, lhs, rhs),
                         _ => self.builder.ins().fadd(lhs, rhs),
                     };
                     if matches!(op, BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge) {
                         let boolean = val;
                         let one = self.builder.ins().iconst(types::I64, 1);
                         let zero = self.builder.ins().iconst(types::I64, 0);
                         let int_val = self.builder.ins().select(boolean, one, zero);
                         (int_val, Type::Boolean)
                     } else {
                         (val, Type::Real)
                     }
                 } else {
                     let val = match op {
                         BinaryOp::Add => self.builder.ins().iadd(lhs, rhs),
                         BinaryOp::Sub => self.builder.ins().isub(lhs, rhs),
                         BinaryOp::Mul => self.builder.ins().imul(lhs, rhs),
                         BinaryOp::Div => self.builder.ins().sdiv(lhs, rhs),
                         BinaryOp::Mod => self.builder.ins().srem(lhs, rhs),
                         BinaryOp::Eq => self.builder.ins().icmp(IntCC::Equal, lhs, rhs),
                         BinaryOp::Ne => self.builder.ins().icmp(IntCC::NotEqual, lhs, rhs),
                         BinaryOp::Lt => self.builder.ins().icmp(IntCC::SignedLessThan, lhs, rhs),
                         BinaryOp::Le => self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, lhs, rhs),
                         BinaryOp::Gt => self.builder.ins().icmp(IntCC::SignedGreaterThan, lhs, rhs),
                         BinaryOp::Ge => self.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, lhs, rhs),
                         BinaryOp::And => self.builder.ins().band(lhs, rhs),
                         BinaryOp::Or => self.builder.ins().bor(lhs, rhs),
                         _ => self.builder.ins().iadd(lhs, rhs),
                     };
                     
                     if matches!(op, BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge) {
                        let one = self.builder.ins().iconst(types::I64, 1);
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let int_val = self.builder.ins().select(val, one, zero);
                        (int_val, Type::Boolean)
                     } else {
                        (val, Type::Integer) 
                     }
                 }
            }
            Expression::Index { array, indices } => {
                // Obtener puntero base del arreglo
                let base_ptr = if let Some(v) = self.variables.get(array) {
                    self.builder.use_var(*v)
                } else {
                    self.builder.ins().iconst(types::I64, 0)
                };
                // Calcular índice plano usando info de dimensiones
                let flat_idx = self.compute_flat_index(array, indices);
                let eight = self.builder.ins().iconst(types::I64, 8);
                let offset = self.builder.ins().imul(flat_idx, eight);
                let addr = self.builder.ins().iadd(base_ptr, offset);
                // Cargar valor
                let val = self.builder.ins().load(types::I64, MemFlags::new(), addr, 0);
                let elem_ty = self.array_elem_types.get(array).cloned().unwrap_or(Type::Integer);
                (val, elem_ty)
            }
            Expression::Call { function, args } => {
                let func_lower = function.to_lowercase();
                
                if let Some(builtin) = lookup_builtin(&func_lower) {
                    let mut sig = self.module.make_signature();
                    for &param_ty in builtin.params {
                        sig.params.push(AbiParam::new(param_ty));
                    }
                    sig.returns.push(AbiParam::new(builtin.ret));
                    
                    let callee = self.module.declare_function(builtin.runtime_name, Linkage::Import, &sig).unwrap();
                    let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                    
                    let mut arg_vals = Vec::new();
                    for (i, arg) in args.iter().enumerate() {
                        let (val, val_ty) = self.translate_expr(arg);
                        let expected_ty = if i < builtin.params.len() { builtin.params[i] } else { types::I64 };
                        
                        let val = if expected_ty == types::F64 && val_ty == Type::Integer {
                            self.builder.ins().fcvt_from_sint(types::F64, val)
                        } else {
                            val
                        };
                        arg_vals.push(val);
                    }
                    
                    let call = self.builder.ins().call(local_callee, &arg_vals);
                    let result = self.builder.inst_results(call)[0];
                    (result, builtin.ret_ast)
                } else if let Some(func_info) = self.user_functions.get(&func_lower) {
                    // Llamada a función definida por el usuario
                    let mut arg_vals = Vec::new();
                    for (i, arg) in args.iter().enumerate() {
                        let (val, val_ty) = self.translate_expr(arg);
                        if i < func_info.params.len() {
                            let expected_ty = &func_info.params[i].1;
                            let val = if *expected_ty == Type::Real && val_ty == Type::Integer {
                                self.builder.ins().fcvt_from_sint(types::F64, val)
                            } else if *expected_ty != Type::Real && val_ty == Type::Real {
                                self.builder.ins().fcvt_to_sint_sat(types::I64, val)
                            } else {
                                val
                            };
                            arg_vals.push(val);
                        } else {
                            arg_vals.push(val);
                        }
                    }

                    let local_callee = self.module.declare_func_in_func(func_info.func_id, self.builder.func);
                    let call = self.builder.ins().call(local_callee, &arg_vals);
                    let results = self.builder.inst_results(call).to_vec();

                    // Manejar valor de retorno
                    let mut result_idx = 0;
                    let (ret_val, ret_ty) = if func_info.has_return && !results.is_empty() {
                        let val = results[0];
                        result_idx = 1;
                        let ty = func_info.return_type.clone().unwrap_or(Type::Integer);
                        (val, ty)
                    } else {
                        (self.builder.ins().iconst(types::I64, 0), Type::Integer)
                    };

                    // Manejar escrituras por referencia
                    for (j, (_, _, by_ref)) in func_info.params.iter().enumerate() {
                        if *by_ref {
                            if result_idx < results.len() {
                                if let Some(Expression::Variable(var_name)) = args.get(j) {
                                    if let Some(var) = self.variables.get(var_name) {
                                        let new_val = results[result_idx];
                                        // Convertir tipo si es necesario
                                        let var_val = self.builder.use_var(*var);
                                        let var_cl_ty = self.builder.func.dfg.value_type(var_val);
                                        let new_cl_ty = self.builder.func.dfg.value_type(new_val);
                                        let new_val = if var_cl_ty == types::I64 && new_cl_ty == types::F64 {
                                            self.builder.ins().fcvt_to_sint_sat(types::I64, new_val)
                                        } else if var_cl_ty == types::F64 && new_cl_ty == types::I64 {
                                            self.builder.ins().fcvt_from_sint(types::F64, new_val)
                                        } else {
                                            new_val
                                        };
                                        self.builder.def_var(*var, new_val);
                                    }
                                }
                            }
                            result_idx += 1;
                        }
                    }

                    (ret_val, ret_ty)
                } else {
                    // Función desconocida
                    (self.builder.ins().iconst(types::I64, 0), Type::Integer)
                }
            }
            Expression::Unary { op, expr } => {
                let (val, ty) = self.translate_expr(expr);
                use crate::ast::UnaryOp;
                match op {
                    UnaryOp::Neg => {
                        if ty == Type::Real {
                            (self.builder.ins().fneg(val), Type::Real)
                        } else {
                            let zero = self.builder.ins().iconst(types::I64, 0);
                            (self.builder.ins().isub(zero, val), Type::Integer)
                        }
                    }
                    UnaryOp::Not => {
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        let cmp = self.builder.ins().icmp(IntCC::Equal, val, zero);
                        let one = self.builder.ins().iconst(types::I64, 1);
                        let zero2 = self.builder.ins().iconst(types::I64, 0);
                        (self.builder.ins().select(cmp, one, zero2), Type::Boolean)
                    }
                }
            }
            _ => (self.builder.ins().iconst(types::I64, 0), Type::Integer),
        }
    }
}

// ============================================================
// Funciones auxiliares de tiempo de ejecución (llamadas desde código JIT)
// ============================================================

// --- E/S ---

extern "C" fn print_int(n: i64) -> i32 {
    print!("{}", n);
    0
}

extern "C" fn print_real(n: f64) -> i32 {
    print!("{}", n);
    0
}

extern "C" fn print_str(s: *const u8) -> i32 {
    let c_str = unsafe { std::ffi::CStr::from_ptr(s as *const i8) };
    if let Ok(s_slice) = c_str.to_str() {
        print!("{}", s_slice);
    }
    0
}

extern "C" fn print_newline() -> i32 {
    println!();
    0
}

extern "C" fn read_int() -> i64 {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap_or(0);
    input.trim().parse().unwrap_or(0)
}

extern "C" fn read_real() -> f64 {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap_or(0);
    input.trim().parse().unwrap_or(0.0)
}

// --- Matemáticas: f64 -> f64 ---

extern "C" fn builtin_power(base: f64, exp: f64) -> f64 { base.powf(exp) }
extern "C" fn builtin_rc(x: f64) -> f64 { x.sqrt() }
extern "C" fn builtin_abs(x: f64) -> f64 { x.abs() }
extern "C" fn builtin_ln(x: f64) -> f64 { x.ln() }
extern "C" fn builtin_exp(x: f64) -> f64 { x.exp() }
extern "C" fn builtin_sen(x: f64) -> f64 { x.sin() }
extern "C" fn builtin_cos(x: f64) -> f64 { x.cos() }
extern "C" fn builtin_tan(x: f64) -> f64 { x.tan() }
extern "C" fn builtin_asen(x: f64) -> f64 { x.asin() }
extern "C" fn builtin_acos(x: f64) -> f64 { x.acos() }
extern "C" fn builtin_atan(x: f64) -> f64 { x.atan() }

// --- Matemáticas: f64 -> i64 ---

extern "C" fn builtin_trunc(x: f64) -> i64 { x as i64 }
extern "C" fn builtin_redon(x: f64) -> i64 { x.round() as i64 }

// --- Aleatorio ---

extern "C" fn builtin_azar(n: i64) -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let mut x = seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x % (n as u64)) as i64
}

extern "C" fn builtin_aleatorio(a: i64, b: i64) -> i64 {
    let range = (b - a + 1).unsigned_abs();
    if range == 0 { return a; }
    a + builtin_azar(range as i64)
}

// --- Funciones de cadena ---

/// Auxiliar: leer un puntero de cadena C en un &str de Rust
unsafe fn cstr_to_str<'a>(ptr: *const u8) -> &'a str {
    if ptr.is_null() { return ""; }
    unsafe {
        std::ffi::CStr::from_ptr(ptr as *const i8)
            .to_str()
            .unwrap_or("")
    }
}

/// Auxiliar: filtrar un String de Rust como puntero de cadena C
fn leak_string(s: String) -> *const u8 {
    let c = std::ffi::CString::new(s).unwrap_or_default();
    let ptr = c.as_ptr() as *const u8;
    std::mem::forget(c);
    ptr
}

extern "C" fn builtin_longitud(s: *const u8) -> i64 {
    unsafe { cstr_to_str(s).len() as i64 }
}

extern "C" fn builtin_mayusculas(s: *const u8) -> *const u8 {
    let upper = unsafe { cstr_to_str(s) }.to_uppercase();
    leak_string(upper)
}

extern "C" fn builtin_minusculas(s: *const u8) -> *const u8 {
    let lower = unsafe { cstr_to_str(s) }.to_lowercase();
    leak_string(lower)
}

extern "C" fn builtin_subcadena(s: *const u8, x: i64, y: i64) -> *const u8 {
    let text = unsafe { cstr_to_str(s) };
    // Las posiciones de PSeInt son base-1 (o base-0 según configuración). Usamos base-0.
    let start = (x as usize).saturating_sub(1);
    let end = y as usize;
    let sub = if start < text.len() {
        &text[start..end.min(text.len())]
    } else {
        ""
    };
    leak_string(sub.to_string())
}

extern "C" fn builtin_concatenar(s1: *const u8, s2: *const u8) -> *const u8 {
    let a = unsafe { cstr_to_str(s1) };
    let b = unsafe { cstr_to_str(s2) };
    leak_string(format!("{}{}", a, b))
}

// --- Conversión ---

extern "C" fn builtin_convertiranumero(s: *const u8) -> f64 {
    let text = unsafe { cstr_to_str(s) };
    text.trim().parse().unwrap_or(0.0)
}

extern "C" fn builtin_convertiratexto(n: f64) -> *const u8 {
    leak_string(format!("{}", n))
}

extern "C" fn builtin_int_to_str(n: i64) -> *const u8 {
    leak_string(format!("{}", n))
}

// --- Tiempo ---

extern "C" fn builtin_horaactual() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Convertir a HHMMSS (aproximación de hora local usando UTC)
    let total_secs = secs % 86400;
    let hh = total_secs / 3600;
    let mm = (total_secs % 3600) / 60;
    let ss = total_secs % 60;
    (hh * 10000 + mm * 100 + ss) as i64
}

extern "C" fn builtin_fechaactual() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // Convertir a AAAAMMDD (aproximación UTC)
    // Días desde la época
    let days = (secs / 86400) as i64;
    // Cálculo simple de calendario
    let mut y = 1970i64;
    let mut remaining = days;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if remaining < days_in_year { break; }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 1i64;
    for &md in &month_days {
        if remaining < md { break; }
        remaining -= md;
        m += 1;
    }
    let d = remaining + 1;
    y * 10000 + m * 100 + d
}

// --- Arreglos ---

extern "C" fn builtin_alloc_array(n: i64) -> i64 {
    let size = n.max(1) as usize;
    let layout = std::alloc::Layout::array::<i64>(size).unwrap();
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    ptr as i64
}

// --- Pantalla / Flush ---

extern "C" fn builtin_clear_screen() -> i32 {
    use std::io::Write;
    print!("\x1b[2J\x1b[H");
    std::io::stdout().flush().unwrap_or(());
    0
}

extern "C" fn flush_stdout() -> i32 {
    use std::io::Write;
    std::io::stdout().flush().unwrap_or(());
    0
}

extern "C" fn builtin_sleep_secs(secs: i64) -> i32 {
    std::thread::sleep(std::time::Duration::from_secs(secs.max(0) as u64));
    0
}

extern "C" fn builtin_sleep_millis(millis: i64) -> i32 {
    std::thread::sleep(std::time::Duration::from_millis(millis.max(0) as u64));
    0
}

extern "C" fn builtin_wait_key() -> i32 {
    use std::io::Read;
    // Leer un byte de stdin (esperar que el usuario presione Enter o una tecla)
    let mut buf = [0u8; 1];
    let _ = std::io::stdin().read(&mut buf);
    0
}
