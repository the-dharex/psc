use cranelift::prelude::*;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module, DataDescription, DataId, FuncId};
use cranelift::codegen::ir::UserFuncName;
use std::collections::HashMap;
use std::sync::Mutex;
use crate::ast::{Program, Statement, Expression, BinaryOp, Type, Function};

struct GcArena {
    /// Punteros de CString (strings dinámicos generados en runtime)
    strings: Vec<*mut u8>,
    /// Pares (ptr, layout) de arreglos asignados con alloc_zeroed
    arrays: Vec<(*mut u8, std::alloc::Layout)>,
}

unsafe impl Send for GcArena {}
unsafe impl Sync for GcArena {}

use std::sync::OnceLock;

static GC: OnceLock<Mutex<GcArena>> = OnceLock::new();

fn gc() -> &'static Mutex<GcArena> {
    GC.get_or_init(|| Mutex::new(GcArena { strings: Vec::new(), arrays: Vec::new() }))
}

/// Registra un puntero de CString en el GC y devuelve el mismo puntero.
fn gc_register_string(ptr: *mut u8) -> *const u8 {
    if let Ok(mut arena) = gc().lock() {
        arena.strings.push(ptr);
    }
    ptr as *const u8
}

/// Registra un arreglo en el GC y devuelve el puntero como i64.
fn gc_register_array(ptr: *mut u8, layout: std::alloc::Layout) -> i64 {
    if let Ok(mut arena) = gc().lock() {
        arena.arrays.push((ptr, layout));
    }
    ptr as i64
}

/// Libera toda la memoria rastreada.
extern "C" fn gc_free_all() {
    if let Ok(mut arena) = gc().lock() {
        // Liberar strings
        for ptr in arena.strings.drain(..) {
            if !ptr.is_null() {
                // SAFETY: ptr fue creado por CString::into_raw() en gc_alloc_string
                unsafe { drop(std::ffi::CString::from_raw(ptr as *mut i8)); }
            }
        }
        // Liberar arreglos
        for (ptr, layout) in arena.arrays.drain(..) {
            if !ptr.is_null() {
                // SAFETY: ptr fue creado por alloc_zeroed con este mismo layout
                unsafe { std::alloc::dealloc(ptr, layout); }
            }
        }
    }
}

/// Asigna un CString rastreado por el GC y devuelve *const u8.
fn gc_alloc_string(s: String) -> *const u8 {
    let s = s.replace('\0', "");
    let c = std::ffi::CString::new(s).unwrap_or_default();
    let ptr = c.into_raw() as *mut u8;   // transfiere ownership al GC
    gc_register_string(ptr)
}

pub mod aot;

/// Selección de nivel de optimización de Cranelift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CraneliftOptLevel {
    None,
    Speed,
    SpeedAndSize,
}

/// Información de una función definida por el usuario
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

        flag_builder.set("is_pic", "false").unwrap();

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
        // GC: liberar toda la memoria rastreada al final del programa
        builder.symbol("gc_free_all", gc_free_all as *const u8);
        
        let module = JITModule::new(builder);
        
        Self {
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            string_literals: HashMap::new(),
        }
    }

    pub fn compile(&mut self, program: &Program) -> Result<*const u8, String> {
        let global_array_dims = extract_constant_array_dims(program);

        // Detectar parámetros de array para cada función
        let func_array_params = detect_array_parameters(program);

        // Declarar todas las funciones de usuario y recopilar info
        let mut user_functions: HashMap<String, UserFuncInfo> = HashMap::new();
        for func in &program.functions {
            let mut sig = self.module.make_signature();
            for (_, ty, _) in &func.params {
                let cl_ty = match ty { Type::Real => types::F64, _ => types::I64 };
                sig.params.push(AbiParam::new(cl_ty));
            }
            
            // Agregar parámetros dimensión para arrays (después de los parámetros normales)
            // Estos se pasan como parámetros normales, no ocultos
            if let Some(array_params) = func_array_params.get(&func.name.to_lowercase()) {
                for (_, dims_count) in array_params {
                    for _ in 0..*dims_count {
                        sig.params.push(AbiParam::new(types::I64));
                    }
                }
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

        for func in &program.functions {
            self.compile_user_function(func, &user_functions, &global_array_dims, &func_array_params)?;
        }

        // Compilar main
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
                func_array_params: &func_array_params,
            };

            for stmt in &program.main_body {
                 trans.translate_stmt(stmt);
            }

            // Llamar al GC para liberar toda la memoria del programa al terminar
            {
                let sig_gc = trans.module.make_signature();
                let gc_callee = trans.module.declare_function("gc_free_all", Linkage::Import, &sig_gc).unwrap();
                let local_gc = trans.module.declare_func_in_func(gc_callee, trans.builder.func);
                trans.builder.ins().call(local_gc, &[]);
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

    fn compile_user_function(&mut self, func: &Function, user_functions: &HashMap<String, UserFuncInfo>, global_array_dims: &HashMap<String, Vec<i64>>, func_array_params: &HashMap<String, Vec<(usize, usize)>>) -> Result<(), String> {
        let info = user_functions.get(&func.name.to_lowercase()).unwrap();

        // Reconstruir firma (con parámetros dimensión ocultos)
        let mut sig = self.module.make_signature();
        for (_, ty, _) in &func.params {
            let cl_ty = match ty { Type::Real => types::F64, _ => types::I64 };
            sig.params.push(AbiParam::new(cl_ty));
        }
        
        // Agregar parámetros dimensión ocultos
        if let Some(array_param_indices) = func_array_params.get(&func.name.to_lowercase()) {
            for (_, dims_count) in array_param_indices {
                for _ in 0..*dims_count {
                    sig.params.push(AbiParam::new(types::I64));
                }
            }
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

            // Extraer dimensiones de los parámetros ocultos
            let mut array_dims_from_params: HashMap<String, Vec<Value>> = HashMap::new();
            let mut dim_param_idx = func.params.len();
            if let Some(array_param_indices) = func_array_params.get(&func.name.to_lowercase()) {
                for (param_idx, dims_count) in array_param_indices.iter() {
                    if let Some((param_name, _, _)) = func.params.get(*param_idx) {
                        let mut dims: Vec<Value> = Vec::new();
                        for _ in 0..*dims_count {
                            if dim_param_idx < block_params.len() {
                                dims.push(block_params[dim_param_idx]);
                                dim_param_idx += 1;
                            }
                        }
                        if dims.len() == *dims_count {
                            array_dims_from_params.insert(param_name.clone(), dims);
                        }
                    }
                }
            }

            let mut trans = FunctionTranslator {
                builder,
                variables,
                variable_types,
                module: &mut self.module,
                string_literals: &mut self.string_literals,
                user_functions,
                array_dims: array_dims_from_params,
                array_elem_types: HashMap::new(),
                global_array_dims,
                func_array_params,
            };

            for stmt in &func.body {
                trans.translate_stmt(stmt);
            }

            // Construir valores de retorno
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

/// Detecta qué parámetros de cada función son arrays y cuántas dimensiones tienen
/// Retorna: HashMap<func_name_lower, HashMap<param_index, num_dimensions>>
fn detect_array_parameters(program: &Program) -> HashMap<String, Vec<(usize, usize)>> {
    let mut result: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    
    for func in &program.functions {
        let param_names: std::collections::HashSet<String> = 
            func.params.iter().map(|(name, _, _)| name.to_lowercase()).collect();
        let mut array_dims: HashMap<String, usize> = HashMap::new();
        
        fn scan_for_array_access(
            stmt: &Statement,
            param_names: &std::collections::HashSet<String>,
            array_dims: &mut HashMap<String, usize>,
        ) {
            match stmt {
                Statement::IndexAssign { array, indices, .. } => {
                    if param_names.contains(&array.to_lowercase()) {
                        array_dims.entry(array.to_lowercase())
                            .and_modify(|d| *d = (*d).max(indices.len()))
                            .or_insert(indices.len());
                    }
                }
                Statement::Assign { value, .. } => {
                    scan_expr_for_array_access(value, param_names, array_dims);
                }
                Statement::If { condition, then_branch, else_branch, .. } => {
                    scan_expr_for_array_access(condition, param_names, array_dims);
                    for s in then_branch { scan_for_array_access(s, param_names, array_dims); }
                    if let Some(eb) = else_branch {
                        for s in eb { scan_for_array_access(s, param_names, array_dims); }
                    }
                }
                Statement::While { condition, body, .. } => {
                    scan_expr_for_array_access(condition, param_names, array_dims);
                    for s in body { scan_for_array_access(s, param_names, array_dims); }
                }
                Statement::Repeat { body, until, .. } => {
                    for s in body { scan_for_array_access(s, param_names, array_dims); }
                    scan_expr_for_array_access(until, param_names, array_dims);
                }
                Statement::For { body, .. } => {
                    for s in body { scan_for_array_access(s, param_names, array_dims); }
                }
                Statement::Write(exprs, _) => {
                    for expr in exprs { scan_expr_for_array_access(expr, param_names, array_dims); }
                }
                Statement::Read(exprs) => {
                    for expr in exprs { scan_expr_for_array_access(expr, param_names, array_dims); }
                }
                _ => {}
            }
        }
        
        fn scan_expr_for_array_access(
            expr: &Expression,
            param_names: &std::collections::HashSet<String>,
            array_dims: &mut HashMap<String, usize>,
        ) {
            match expr {
                Expression::Index { array, indices } => {
                    if param_names.contains(&array.to_lowercase()) {
                        array_dims.entry(array.to_lowercase())
                            .and_modify(|d| *d = (*d).max(indices.len()))
                            .or_insert(indices.len());
                    }
                }
                Expression::Binary { left, right, .. } => {
                    scan_expr_for_array_access(left, param_names, array_dims);
                    scan_expr_for_array_access(right, param_names, array_dims);
                }
                Expression::Unary { expr, .. } => {
                    scan_expr_for_array_access(expr, param_names, array_dims);
                }
                Expression::Call { args, .. } => {
                    for arg in args { scan_expr_for_array_access(arg, param_names, array_dims); }
                }
                _ => {}
            }
        }
        
        for stmt in &func.body {
            scan_for_array_access(stmt, &param_names, &mut array_dims);
        }
        
        // Convertir a Vec de (índice_parámetro, cantidad_dimensiones) - mantiene orden
        let mut param_array_list: Vec<(usize, usize)> = Vec::new();
        for (idx, (name, _, _)) in func.params.iter().enumerate() {
            if let Some(dims) = array_dims.get(&name.to_lowercase()) {
                param_array_list.push((idx, *dims));
            }
        }
        
        if !param_array_list.is_empty() {
            result.insert(func.name.to_lowercase(), param_array_list);
        }
    }
    
    result
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
    pub(crate) func_array_params: &'a HashMap<String, Vec<(usize, usize)>>,
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
    fn call_runtime(
        &mut self,
        name: &str,
        params: &[types::Type],
        ret: Option<types::Type>,
        args: &[Value],
    ) -> Option<Value> {
        let mut sig = self.module.make_signature();
        for &p in params {
            sig.params.push(AbiParam::new(p));
        }
        if let Some(r) = ret {
            sig.returns.push(AbiParam::new(r));
        }
        let callee = self.module
            .declare_function(name, Linkage::Import, &sig)
            .expect("declare_function failed");
        let local = self.module.declare_func_in_func(callee, self.builder.func);
        let call = self.builder.ins().call(local, args);
        if ret.is_some() {
            Some(self.builder.inst_results(call)[0])
        } else {
            None
        }
    }

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
        // FIX #12: Usar call_runtime en lugar de repetir la construcción de firma
        if *ty == Type::Real {
            self.call_runtime("builtin_convertiratexto", &[types::F64], Some(types::I64), &[val])
                .unwrap()
        } else {
            self.call_runtime("builtin_int_to_str", &[types::I64], Some(types::I64), &[val])
                .unwrap()
        }
    }

    fn compute_flat_index(&mut self, array: &str, indices: &[Expression]) -> Value {
        let one = self.builder.ins().iconst(types::I64, 1);
        let zero_val = self.builder.ins().iconst(types::I64, 0);

        let clamp = |builder: &mut FunctionBuilder, idx: Value, dim: Value| -> Value {
            let one = builder.ins().iconst(types::I64, 1);
            let max_idx = builder.ins().isub(dim, one);
            let zero = builder.ins().iconst(types::I64, 0);
            let is_neg = builder.ins().icmp(IntCC::SignedLessThan, idx, zero);
            
            let clamped_lo = builder.ins().select(is_neg, zero, idx);
            let is_over = builder.ins().icmp(IntCC::SignedGreaterThan, clamped_lo, max_idx);
            builder.ins().select(is_over, max_idx, clamped_lo)
        };

        if indices.len() == 1 {
            let (idx, _) = self.translate_expr(&indices[0]);
            let idx_zero = self.builder.ins().isub(idx, one);
            // If we know the static dimension, clamp; otherwise just saturate at 0
            if let Some(sizes) = self.global_array_dims.get(&array.to_lowercase()) {
                let dim = self.builder.ins().iconst(types::I64, sizes[0]);
                clamp(&mut self.builder, idx_zero, dim)
            } else if let Some(dims) = self.array_dims.get(&array.to_lowercase()).cloned() {
                clamp(&mut self.builder, idx_zero, dims[0])
            } else {
                // Unknown size: at least clamp to non-negative
                let is_neg = self.builder.ins().icmp(IntCC::SignedLessThan, idx_zero, zero_val);
                self.builder.ins().select(is_neg, zero_val, idx_zero)
            }
        } else {
            // Multi-dimensional: look up dims
            let dims: Vec<Value> = if let Some(d) = self.array_dims.get(&array.to_lowercase()) {
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
                // Clamp if dim info is available
                let idx_safe = if k < dims.len() {
                    clamp(&mut self.builder, idx_zero, dims[k])
                } else {
                    let is_neg = self.builder.ins().icmp(IntCC::SignedLessThan, idx_zero, zero_val);
                    self.builder.ins().select(is_neg, zero_val, idx_zero)
                };
                let mut stride = self.builder.ins().iconst(types::I64, 1);
                for dim_val in dims.iter().skip(k + 1) {
                    stride = self.builder.ins().imul(stride, *dim_val);
                }
                let contrib = self.builder.ins().imul(idx_safe, stride);
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
                for var in vars.iter() {
                    if !self.variables.contains_key(var) {
                        let variable = Variable::new(self.variables.len());
                        self.builder.declare_var(variable, cl_type);
                        // FIX #8: Inicializar con valor cero para que use_var no falle
                        let init_val = match ty {
                            Type::Real => self.builder.ins().f64const(0.0),
                            _ => self.builder.ins().iconst(cl_type, 0),
                        };
                        self.builder.def_var(variable, init_val);
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
                 self.array_dims.insert(name.to_lowercase(), dim_vals);
                 // Llamar al asignador en tiempo de ejecución
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
                 let base_ptr = if let Some(v) = self.variables.get(&array.to_lowercase()) {
                     self.builder.use_var(*v)
                 } else if let Some(v) = self.variables.get(array) {
                     self.builder.use_var(*v)
                 } else {
                     self.builder.ins().iconst(types::I64, 0)
                 };
                 // Calcular índice plano usando info de dimensiones
                 let flat_idx = self.compute_flat_index(&array.to_lowercase(), indices);
                 let eight = self.builder.ins().iconst(types::I64, 8);
                 let offset = self.builder.ins().imul(flat_idx, eight);
                 let addr = self.builder.ins().iadd(base_ptr, offset);
                 // Traducir valor
                 let (val, val_ty) = self.translate_expr(value);
                 // Rastrear tipo de elemento
                 self.array_elem_types.entry(array.to_lowercase()).or_insert(val_ty.clone());
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
                            let is_real_array = self.array_elem_types.get(array)
                                .map(|t| *t == Type::Real)
                                .unwrap_or(false);

                            let ret_cl_ty = if is_real_array { types::F64 } else { types::I64 };
                            let read_fn = if is_real_array { "read_real" } else { "read_int" };

                            let mut sig = self.module.make_signature();
                            sig.returns.push(AbiParam::new(ret_cl_ty));
                            let callee = self.module.declare_function(read_fn, Linkage::Import, &sig).unwrap();
                            let local_callee = self.module.declare_func_in_func(callee, self.builder.func);
                            let call = self.builder.ins().call(local_callee, &[]);
                            let result = self.builder.inst_results(call)[0];

                            // Convertir a i64 para almacenamiento (reales se guardan como bitcast)
                            let store_val = if is_real_array {
                                self.builder.ins().bitcast(types::I64, MemFlags::new(), result)
                            } else {
                                result
                            };

                            let base_ptr = if let Some(v) = self.variables.get(array) {
                                self.builder.use_var(*v)
                            } else {
                                self.builder.ins().iconst(types::I64, 0)
                            };
                            let flat_idx = self.compute_flat_index(array, indices);
                            let eight = self.builder.ins().iconst(types::I64, 8);
                            let offset = self.builder.ins().imul(flat_idx, eight);
                            let addr = self.builder.ins().iadd(base_ptr, offset);
                            self.builder.ins().store(MemFlags::new(), store_val, addr, 0);
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

                // Cuerpo
                self.builder.switch_to_block(body_block);
                self.builder.seal_block(body_block);
                for stmt in body {
                    self.translate_stmt(stmt);
                }

                self.builder.ins().jump(header_block, &[]);
                self.builder.seal_block(header_block);

                self.builder.switch_to_block(exit_block);
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
                // Coercer start_val al tipo de la variable de loop si ya existe con otro tipo
                let loop_var_val = self.builder.use_var(loop_var);
                let loop_cl_ty = self.builder.func.dfg.value_type(loop_var_val);
                let start_cl_ty = self.builder.func.dfg.value_type(start_val);
                let start_val = if loop_cl_ty == types::F64 && start_cl_ty == types::I64 {
                    self.builder.ins().fcvt_from_sint(types::F64, start_val)
                } else if loop_cl_ty == types::I64 && start_cl_ty == types::F64 {
                    self.builder.ins().fcvt_to_sint_sat(types::I64, start_val)
                } else { start_val };
                self.builder.def_var(loop_var, start_val);

                let loop_is_real = loop_cl_ty == types::F64 || start_ty == Type::Real;

                // Evaluar paso (por defecto 1 o 1.0 según tipo)
                let step_val = if let Some(step_expr) = step {
                    let (sv, sv_ty) = self.translate_expr(step_expr);
                    // Coercer step al tipo del loop
                    if loop_is_real && sv_ty == Type::Integer {
                        self.builder.ins().fcvt_from_sint(types::F64, sv)
                    } else if !loop_is_real && sv_ty == Type::Real {
                        self.builder.ins().fcvt_to_sint_sat(types::I64, sv)
                    } else { sv }
                } else if loop_is_real {
                    self.builder.ins().f64const(1.0)
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
                let (end_raw, end_ty) = self.translate_expr(end);
                let end_val = if loop_is_real && end_ty == Type::Integer {
                    self.builder.ins().fcvt_from_sint(types::F64, end_raw)
                } else if !loop_is_real && end_ty == Type::Real {
                    self.builder.ins().fcvt_to_sint_sat(types::I64, end_raw)
                } else { end_raw };

                // Determinar condición según tipo
                let cond = if loop_is_real {
                    // Para reales sólo soportar paso positivo
                    self.builder.ins().fcmp(FloatCC::LessThanOrEqual, current, end_val)
                } else {
                    let zero = self.builder.ins().iconst(types::I64, 0);
                    let step_positive = self.builder.ins().icmp(IntCC::SignedGreaterThan, step_val, zero);
                    let cond_le = self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, current, end_val);
                    let cond_ge = self.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, current, end_val);
                    self.builder.ins().select(step_positive, cond_le, cond_ge)
                };

                self.builder.ins().brif(cond, body_block, &[], exit_block, &[]);

                // Cuerpo
                self.builder.switch_to_block(body_block);
                self.builder.seal_block(body_block);
                for stmt in body {
                    self.translate_stmt(stmt);
                }
                // Incrementar: var <- var + paso (con el tipo correcto)
                let current_after = self.builder.use_var(loop_var);
                let next = if loop_is_real {
                    self.builder.ins().fadd(current_after, step_val)
                } else {
                    self.builder.ins().iadd(current_after, step_val)
                };
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
                // FIX #12: Usar call_runtime helper
                self.call_runtime("builtin_clear_screen", &[], Some(types::I32), &[]);
            }
            Statement::Wait { duration, milliseconds } => {
                let (dur_val, _) = self.translate_expr(duration);
                let func_name = if *milliseconds { "builtin_sleep_millis" } else { "builtin_sleep_secs" };
                // FIX #12: Usar call_runtime helper
                self.call_runtime(func_name, &[types::I64], Some(types::I32), &[dur_val]);
            }
            Statement::WaitKey => {
                // FIX #12: Usar call_runtime helper
                self.call_runtime("builtin_wait_key", &[], Some(types::I32), &[]);
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
                    //eprintln!("[PSeInt JIT] Advertencia: variable '{}' usada sin declarar; se asume 0.", name);
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
                        BinaryOp::Div => {
                            let zero = self.builder.ins().iconst(types::I64, 0);
                            let one = self.builder.ins().iconst(types::I64, 1);
                            
                            let is_zero = self.builder.ins().icmp(IntCC::Equal, rhs, zero);
                            let safe_rhs = self.builder.ins().select(is_zero, one, rhs);
                            let result = self.builder.ins().sdiv(lhs, safe_rhs);
                            
                            self.builder.ins().select(is_zero, zero, result)
                        }
                        BinaryOp::Mod => {
                            let zero = self.builder.ins().iconst(types::I64, 0);
                            let one = self.builder.ins().iconst(types::I64, 1);
                            
                            let is_zero = self.builder.ins().icmp(IntCC::Equal, rhs, zero);
                            let safe_rhs = self.builder.ins().select(is_zero, one, rhs);
                            let result = self.builder.ins().srem(lhs, safe_rhs);
                            
                            self.builder.ins().select(is_zero, zero, result)
                        }
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
                let base_ptr = if let Some(v) = self.variables.get(&array.to_lowercase()) {
                    self.builder.use_var(*v)
                } else if let Some(v) = self.variables.get(array) {
                    self.builder.use_var(*v)
                } else {
                    self.builder.ins().iconst(types::I64, 0)
                };
                // Calcular índice plano usando info de dimensiones
                let flat_idx = self.compute_flat_index(&array.to_lowercase(), indices);
                let eight = self.builder.ins().iconst(types::I64, 8);
                let offset = self.builder.ins().imul(flat_idx, eight);
                let addr = self.builder.ins().iadd(base_ptr, offset);
                // Cargar valor
                let val = self.builder.ins().load(types::I64, MemFlags::new(), addr, 0);
                let elem_ty = self.array_elem_types.get(&array.to_lowercase()).cloned().unwrap_or(Type::Integer);
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

                    // Agregar dimensiones de arrays como parámetros ocultos
                    if let Some(array_param_indices) = self.func_array_params.get(&func_lower) {
                        for (param_idx, dims_count) in array_param_indices {
                            // Obtener la variable del argumento correspondiente
                            if let Some(arg_expr) = args.get(*param_idx) {
                                if let Expression::Variable(var_name) = arg_expr {
                                    // Obtener dimensiones de la variable del argumento
                                    if let Some(dims) = self.array_dims.get(&var_name.to_lowercase()) {
                                        for dim in dims.iter().take(*dims_count) {
                                            arg_vals.push(*dim);
                                        }
                                    } else if let Some(sizes) = self.global_array_dims.get(&var_name.to_lowercase()) {
                                        for size in sizes.iter().take(*dims_count) {
                                            let dim_val = self.builder.ins().iconst(types::I64, *size);
                                            arg_vals.push(dim_val);
                                        }
                                    } else {
                                        // Si no se encuentran dimensiones, pasar valores por defecto
                                        for _ in 0..*dims_count {
                                            arg_vals.push(self.builder.ins().iconst(types::I64, 1));
                                        }
                                    }
                                } else {
                                    // Si el argumento no es una variable simple, pasar valores por defecto
                                    for _ in 0..*dims_count {
                                        arg_vals.push(self.builder.ins().iconst(types::I64, 1));
                                    }
                                }
                            }
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
                    // eprintln!("[PSeInt JIT] Advertencia: función '{}' no encontrada; se devuelve 0.", function);
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
        }
    }
}

// ============================================================
// Funciones auxiliares de tiempo de ejecución
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

/// Estado persistente del PRNG XorShift64.
static RNG_STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn xorshift64_next() -> u64 {
    use std::sync::atomic::Ordering;
    // Si el estado es 0 (primera llamada), sembrar desde el reloj del sistema
    let mut state = RNG_STATE.load(Ordering::Relaxed);
    if state == 0 {
        state = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(12345678);
        // Garantizar que el estado nunca sea 0 (valor inválido para XorShift)
        if state == 0 { state = 1; }
    }
    // Avance XorShift64
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    RNG_STATE.store(state, Ordering::Relaxed);
    state
}

extern "C" fn builtin_azar(n: i64) -> i64 {
    if n <= 0 { return 0; }
    (xorshift64_next() % (n as u64)) as i64
}

extern "C" fn builtin_aleatorio(a: i64, b: i64) -> i64 {
    let range = (b - a + 1).max(1) as u64;
    a + (xorshift64_next() % range) as i64
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

extern "C" fn builtin_longitud(s: *const u8) -> i64 {
    // Longitud en caracteres Unicode, no en bytes (correcto para PSeInt)
    unsafe { cstr_to_str(s) }.chars().count() as i64
}

extern "C" fn builtin_mayusculas(s: *const u8) -> *const u8 {
    // FIX #1: Usar gc_alloc_string en lugar de leak_string
    let upper = unsafe { cstr_to_str(s) }.to_uppercase();
    gc_alloc_string(upper)
}

extern "C" fn builtin_minusculas(s: *const u8) -> *const u8 {
    let lower = unsafe { cstr_to_str(s) }.to_lowercase();
    gc_alloc_string(lower)
}

extern "C" fn builtin_subcadena(s: *const u8, x: i64, y: i64) -> *const u8 {
    let text = unsafe { cstr_to_str(s) };

    let start = (x as usize).saturating_sub(1);
    let end = y as usize;
    let sub: String = text.chars().skip(start).take(end.saturating_sub(start)).collect();

    gc_alloc_string(sub)
}

extern "C" fn builtin_concatenar(s1: *const u8, s2: *const u8) -> *const u8 {
    let a = unsafe { cstr_to_str(s1) };
    let b = unsafe { cstr_to_str(s2) };

    gc_alloc_string(format!("{}{}", a, b))
}

// --- Conversión ---

extern "C" fn builtin_convertiranumero(s: *const u8) -> f64 {
    let text = unsafe { cstr_to_str(s) };
    text.trim().parse().unwrap_or(0.0)
}

extern "C" fn builtin_convertiratexto(n: f64) -> *const u8 {
    gc_alloc_string(format!("{}", n))
}

extern "C" fn builtin_int_to_str(n: i64) -> *const u8 {
    gc_alloc_string(format!("{}", n))
}

// --- Tiempo ---

/// Obtiene el offset UTC local en segundos.
fn local_utc_offset_secs() -> i64 {
    if let Ok(val) = std::env::var("PSINT_UTC_OFFSET") {
        if let Ok(h) = val.trim().parse::<i64>() {
            return h * 3600;
        }
    }
    // Intentar determinar el offset real usando la hora local del sistema
    #[cfg(unix)]
    {
        extern "C" {
            fn time(t: *mut i64) -> i64;
        }
        let _ = unsafe { time(std::ptr::null_mut()) }; // dummy para compilación
    }
    // Fallback: UTC-4
    -4 * 3600
}

extern "C" fn builtin_horaactual() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let utc_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let local_secs = utc_secs + local_utc_offset_secs();
    let day_secs = local_secs.rem_euclid(86400);
    let hh = day_secs / 3600;
    let mm = (day_secs % 3600) / 60;
    let ss = day_secs % 60;
    hh * 10000 + mm * 100 + ss
}

extern "C" fn builtin_fechaactual() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let utc_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let local_secs = utc_secs + local_utc_offset_secs();
    let days = local_secs.div_euclid(86400);
    // Calcular año, mes, día desde días epoch (1970-01-01)
    let mut y = 1970i64;
    let mut remaining = days;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if remaining < days_in_year { break; }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days: [i64; 12] = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
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
    let layout = std::alloc::Layout::array::<i64>(size)
        .expect("Layout inválido para arreglo");
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        eprintln!("[PSeInt JIT] Error crítico: no se pudo asignar arreglo de {} elementos.", size);
        return 0;
    }
    gc_register_array(ptr, layout)
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