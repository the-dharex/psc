
use cranelift::prelude::*;
use cranelift_object::{ObjectBuilder, ObjectModule};
use cranelift_module::{Linkage, Module, DataId};
use cranelift::codegen::ir::UserFuncName;
use std::collections::HashMap;
use crate::ast::{Program, Type, Function};
use super::{CraneliftOptLevel, FunctionTranslator, UserFuncInfo, extract_constant_array_dims};

/// Generador de código AOT (Ahead-Of-Time) que produce un archivo objeto.
pub struct AotCodeGenerator {
    builder_context: FunctionBuilderContext,
    ctx: codegen::Context,
    module: ObjectModule,
    string_literals: HashMap<String, DataId>,
}

impl AotCodeGenerator {
    pub fn with_opt_level(opt: CraneliftOptLevel) -> Self {
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

        let isa_builder = cranelift_native::builder().unwrap_or_else(|msg| {
            panic!("ISA de la máquina host no soportada: {}", msg);
        });
        let isa = isa_builder.finish(settings::Flags::new(flag_builder)).unwrap();

        let obj_builder = ObjectBuilder::new(
            isa,
            "psc_output",
            cranelift_module::default_libcall_names(),
        ).unwrap();
        let module = ObjectModule::new(obj_builder);

        Self {
            builder_context: FunctionBuilderContext::new(),
            ctx: module.make_context(),
            module,
            string_literals: HashMap::new(),
        }
    }

    /// Compila el programa PSeInt a código objeto. Consume el generador y devuelve los bytes del .obj/.o.
    pub fn compile(mut self, program: &Program) -> Result<Vec<u8>, String> {
        let global_array_dims = extract_constant_array_dims(program);

        // 1. Declarar todas las funciones de usuario
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

        // 3. Compilar main como "psc_main" (la función C main() del runtime lo invocará)
        let mut sig_main = self.module.make_signature();
        sig_main.returns.push(AbiParam::new(types::I32));

        let main_id = self.module.declare_function(
            "psc_main",
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

            let empty_func_array_params: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
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
                func_array_params: &empty_func_array_params,
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

        // Finalizar y emitir código objeto
        let product = self.module.finish();
        product.emit().map_err(|e| e.to_string())
    }

    fn compile_user_function(
        &mut self,
        func: &Function,
        user_functions: &HashMap<String, UserFuncInfo>,
        global_array_dims: &HashMap<String, Vec<i64>>,
    ) -> Result<(), String> {
        let info = user_functions.get(&func.name.to_lowercase()).unwrap();

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

            for (i, (name, ty, _)) in func.params.iter().enumerate() {
                let var = Variable::new(variables.len());
                let cl_ty = match ty { Type::Real => types::F64, _ => types::I64 };
                builder.declare_var(var, cl_ty);
                builder.def_var(var, block_params[i]);
                variables.insert(name.clone(), var);
                variable_types.insert(name.clone(), ty.clone());
            }

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

            let empty_func_array_params: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
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
                func_array_params: &empty_func_array_params,
            };

            for stmt in &func.body {
                trans.translate_stmt(stmt);
            }

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

// ============================================================
// Enlazado: compilar runtime C + objeto → ejecutable
// ============================================================

/// Enlaza el código objeto generado con el runtime C para producir un ejecutable nativo.
pub fn link_executable(obj_bytes: &[u8], output_path: &str) -> Result<(), String> {
    let temp_dir = std::env::temp_dir();
    let obj_ext = if cfg!(target_os = "windows") { "obj" } else { "o" };
    let obj_path = temp_dir.join(format!("psc_output.{}", obj_ext));
    let runtime_path = temp_dir.join("psc_runtime.c");

    // Escribir archivo objeto
    std::fs::write(&obj_path, obj_bytes)
        .map_err(|e| format!("Error escribiendo archivo objeto: {}", e))?;

    // Escribir runtime C
    std::fs::write(&runtime_path, RUNTIME_C)
        .map_err(|e| format!("Error escribiendo runtime C: {}", e))?;

    // Asegurar extensión correcta en el ejecutable de salida
    let output_path = if cfg!(target_os = "windows") && !output_path.ends_with(".exe") {
        format!("{}.exe", output_path)
    } else {
        output_path.to_string()
    };

    // Intentar enlazar con diferentes compiladores (en orden de preferencia)
    let link_result = try_link_clang(&obj_path, &runtime_path, &output_path)
        .or_else(|_| try_link_gcc(&obj_path, &runtime_path, &output_path))
        .or_else(|_| try_link_cc(&obj_path, &runtime_path, &output_path))
        .or_else(|e_prev| {
            if cfg!(target_os = "windows") {
                try_link_msvc(&obj_path, &runtime_path, &output_path)
                    .map_err(|e3| format!("{}\n{}", e_prev, e3))
            } else {
                Err(e_prev)
            }
        });

    // Limpiar temporales
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&runtime_path);

    match link_result {
        Ok(()) => Ok(()),
        Err(e) => Err(format!(
            "No se pudo enlazar el ejecutable.\nAsegúrate de tener un compilador C instalado: clang, gcc, cc o MSVC.\nDetalles:\n{}",
            e
        )),
    }
}

fn try_link_gcc(obj_path: &std::path::Path, runtime_path: &std::path::Path, output: &str) -> Result<(), String> {
    use std::process::Command;
    
    // Flags específicos por plataforma
    let (extra_flags, link_flags): (&[&str], &[&str]) = if cfg!(target_os = "macos") {
        (&["-fPIC"], &["-lm"])
    } else if cfg!(target_os = "windows") {
        (&[], &["-lm", "-lpthread"])
    } else {
        // Linux y otras POSIX
        (&["-fPIC"], &["-lm", "-lpthread"])
    };
    
    let status = Command::new("gcc")
        .args(extra_flags)
        .arg(runtime_path)
        .arg(obj_path)
        .arg("-o")
        .arg(output)
        .args(link_flags)
        .output()
        .map_err(|e| format!("gcc no encontrado: {}", e))?;

    if status.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&status.stderr);
        Err(format!("gcc falló: {}", stderr))
    }
}

fn try_link_cc(obj_path: &std::path::Path, runtime_path: &std::path::Path, output: &str) -> Result<(), String> {
    use std::process::Command;
    
    let (extra_flags, link_flags): (&[&str], &[&str]) = if cfg!(target_os = "macos") {
        (&["-fPIC"], &["-lm"])
    } else if cfg!(target_os = "windows") {
        (&[], &["-lm", "-lpthread"])
    } else {
        (&["-fPIC"], &["-lm", "-lpthread"])
    };
    
    let status = Command::new("cc")
        .args(extra_flags)
        .arg(runtime_path)
        .arg(obj_path)
        .arg("-o")
        .arg(output)
        .args(link_flags)
        .output()
        .map_err(|e| format!("cc no encontrado: {}", e))?;

    if status.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&status.stderr);
        Err(format!("cc falló: {}", stderr))
    }
}

fn try_link_clang(obj_path: &std::path::Path, runtime_path: &std::path::Path, output: &str) -> Result<(), String> {
    use std::process::Command;
    
    let (extra_flags, link_flags): (&[&str], &[&str]) = if cfg!(target_os = "macos") {
        (&["-fPIC"], &["-lm"])
    } else if cfg!(target_os = "windows") {
        (&[], &["-lm", "-lpthread"])
    } else {
        (&["-fPIC"], &["-lm", "-lpthread"])
    };
    
    let status = Command::new("clang")
        .args(extra_flags)
        .arg(runtime_path)
        .arg(obj_path)
        .arg("-o")
        .arg(output)
        .args(link_flags)
        .output()
        .map_err(|e| format!("clang no encontrado: {}", e))?;

    if status.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&status.stderr);
        Err(format!("clang falló: {}", stderr))
    }
}

#[cfg(target_os = "windows")]
fn try_link_msvc(obj_path: &std::path::Path, runtime_path: &std::path::Path, output: &str) -> Result<(), String> {
    use std::process::Command;
    let status = Command::new("cl")
        .arg("/nologo")
        .arg(format!("/Fe:{}", output))
        .arg(runtime_path)
        .arg(obj_path)
        .output()
        .map_err(|e| format!("cl.exe no encontrado: {}", e))?;

    if status.status.success() {
        // Limpiar archivos .obj intermedios que cl genera
        let runtime_obj = runtime_path.with_extension("obj");
        let _ = std::fs::remove_file(&runtime_obj);
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&status.stderr);
        Err(format!("cl.exe falló: {}", stderr))
    }
}

#[cfg(not(target_os = "windows"))]
fn try_link_msvc(_obj_path: &std::path::Path, _runtime_path: &std::path::Path, _output: &str) -> Result<(), String> {
    Err("MSVC no disponible en esta plataforma".to_string())
}

// ============================================================
// Runtime C embebido — se compila y enlaza con el código objeto
// ============================================================

const RUNTIME_C: &str = r#"
/* ================================================================
 * PSeInt Compiler — Runtime de soporte para ejecutables nativos
 * Generado automáticamente. No modificar.
 * ================================================================ */

#include <stdio.h>
#include <stdlib.h>
#include <math.h>
#include <string.h>
#include <ctype.h>
#include <time.h>

#ifdef _WIN32
  #include <windows.h>
#else
  #include <unistd.h>
#endif

/* ================================================================
 * Garbage Collector
 * ================================================================ */

#define PSC_GC_MAX_ALLOCS 100000
#define PSC_GC_MAX_GENERATIONS 10
#define PSC_GC_TEMP_THRESHOLD 1024
#define PSC_STRING_TABLE_SIZE 16384

typedef enum { GC_PERMANENT, GC_TEMPORARY } GcGeneration;

typedef struct {
    void* ptr;
    size_t size;
    GcGeneration gen;
    unsigned int age;
} GcAlloc;

typedef struct {
    const char* str;
    unsigned int refcount;
} GcStringEntry;

typedef struct {
    GcAlloc* items;
    size_t count;
    size_t capacity;
    size_t total_bytes;
    size_t temp_bytes;
    
    GcStringEntry* string_table;
    unsigned int string_entries;
} GcPool;

static GcPool _psc_gc = {NULL, 0, 0, 0, 0, NULL, 0};

void _psc_gc_init(void) {
    if (!_psc_gc.items) {
        _psc_gc.capacity = 1024;
        _psc_gc.items = (GcAlloc*)malloc(sizeof(GcAlloc) * _psc_gc.capacity);
        memset(_psc_gc.items, 0, sizeof(GcAlloc) * _psc_gc.capacity);
        
        _psc_gc.string_table = (GcStringEntry*)malloc(sizeof(GcStringEntry) * PSC_STRING_TABLE_SIZE);
        memset(_psc_gc.string_table, 0, sizeof(GcStringEntry) * PSC_STRING_TABLE_SIZE);
        
        _psc_gc.count = 0;
        _psc_gc.total_bytes = 0;
        _psc_gc.temp_bytes = 0;
        _psc_gc.string_entries = 0;
    }
}

static unsigned int _psc_hash_string(const char* s) {
    unsigned int h = 5381;
    while (*s) h = ((h << 5) + h) ^ (unsigned char)*s++;
    return h % PSC_STRING_TABLE_SIZE;
}

static void _psc_gc_collect_temporaries(void) {
    if (_psc_gc.temp_bytes < PSC_GC_TEMP_THRESHOLD) return;
    
    size_t freed = 0;
    for (size_t i = 0; i < _psc_gc.count; i++) {
        if (_psc_gc.items[i].ptr && _psc_gc.items[i].gen == GC_TEMPORARY) {
            _psc_gc.items[i].age++;
            if (_psc_gc.items[i].age > PSC_GC_MAX_GENERATIONS) {
                free(_psc_gc.items[i].ptr);
                freed += _psc_gc.items[i].size;
                _psc_gc.items[i].ptr = NULL;
                _psc_gc.items[i].size = 0;
            }
        }
    }
    
    _psc_gc.temp_bytes -= freed;
}

void* _psc_gc_malloc(size_t size) {
    _psc_gc_init();
    
    if (_psc_gc.count >= PSC_GC_MAX_ALLOCS - 1) {
        _psc_gc_collect_temporaries();
    }
    
    void* ptr = malloc(size);
    if (!ptr) return NULL;
    
    if (_psc_gc.count >= _psc_gc.capacity) {
        _psc_gc.capacity *= 2;
        if (_psc_gc.capacity > PSC_GC_MAX_ALLOCS) _psc_gc.capacity = PSC_GC_MAX_ALLOCS;
        _psc_gc.items = (GcAlloc*)realloc(_psc_gc.items, sizeof(GcAlloc) * _psc_gc.capacity);
    }
    
    _psc_gc.items[_psc_gc.count].ptr = ptr;
    _psc_gc.items[_psc_gc.count].size = size;
    _psc_gc.items[_psc_gc.count].gen = GC_TEMPORARY;
    _psc_gc.items[_psc_gc.count].age = 0;
    _psc_gc.count++;
    
    _psc_gc.temp_bytes += size;
    _psc_gc.total_bytes += size;
    
    return ptr;
}

const char* _psc_gc_intern_string(const char* s, size_t len) {
    if (!s || len == 0) return "";
    
    unsigned int h = 5381;
    for (size_t i = 0; i < len; i++) {
        h = ((h << 5) + h) ^ (unsigned char)s[i];
    }
    h = h % PSC_STRING_TABLE_SIZE;
    
    for (size_t i = h; i < PSC_STRING_TABLE_SIZE; i++) {
        if (!_psc_gc.string_table[i].str) break;
        if (strncmp(_psc_gc.string_table[i].str, s, len) == 0 &&
            _psc_gc.string_table[i].str[len] == '\0') {
            _psc_gc.string_table[i].refcount++;
            return _psc_gc.string_table[i].str;
        }
    }
    
    char* p = (char*)_psc_gc_malloc(len + 1);
    if (!p) return "";
    memcpy(p, s, len);
    p[len] = '\0';
    
    for (size_t i = h; i < PSC_STRING_TABLE_SIZE; i++) {
        if (!_psc_gc.string_table[i].str) {
            _psc_gc.string_table[i].str = p;
            _psc_gc.string_table[i].refcount = 1;
            _psc_gc.string_entries++;
            return p;
        }
    }
    
    return p;
}

void _psc_gc_cleanup(void) {
    if (!_psc_gc.items) return;
    
    for (size_t i = 0; i < _psc_gc.count; i++) {
        if (_psc_gc.items[i].ptr) {
            free(_psc_gc.items[i].ptr);
        }
    }
    free(_psc_gc.items);
    free(_psc_gc.string_table);
    _psc_gc.items = NULL;
    _psc_gc.string_table = NULL;
    _psc_gc.count = 0;
}

/* Punto de entrada del programa PSeInt compilado */
extern int psc_main(void);

int main(void) {
    _psc_gc_init();
    int result = psc_main();
    _psc_gc_cleanup();
    printf("Programa termino con: %d\n", result);
    return 0;
}

/* ---- E/S ---- */

int print_int(long long n) {
    printf("%lld", n);
    return 0;
}

int print_real(double n) {
    printf("%g", n);
    return 0;
}

int print_str(const char* s) {
    if (s) printf("%s", s);
    return 0;
}

int print_newline(void) {
    printf("\n");
    return 0;
}

long long read_int(void) {
    char buf[256];
    if (fgets(buf, sizeof(buf), stdin)) {
        return atoll(buf);
    }
    return 0;
}

double read_real(void) {
    char buf[256];
    if (fgets(buf, sizeof(buf), stdin)) {
        return atof(buf);
    }
    return 0.0;
}

/* ---- Matem\xC3\xA1ticas: f64 -> f64 ---- */

double builtin_power(double base, double e) { return pow(base, e); }
double builtin_rc(double x)    { return sqrt(x); }
double builtin_abs(double x)   { return fabs(x); }
double builtin_ln(double x)    { return log(x);  }
double builtin_exp(double x)   { return exp(x);  }
double builtin_sen(double x)   { return sin(x);  }
double builtin_cos(double x)   { return cos(x);  }
double builtin_tan(double x)   { return tan(x);  }
double builtin_asen(double x)  { return asin(x); }
double builtin_acos(double x)  { return acos(x); }
double builtin_atan(double x)  { return atan(x); }

/* ---- Matem\xC3\xA1ticas: f64 -> i64 ---- */

long long builtin_trunc(double x) { return (long long)x; }
long long builtin_redon(double x) { return (long long)round(x); }

/* ---- Aleatorio ---- */

static int _psc_seeded = 0;

long long builtin_azar(long long n) {
    if (!_psc_seeded) { srand((unsigned)time(NULL)); _psc_seeded = 1; }
    if (n <= 0) return 0;
    return (long long)(rand() % (int)n);
}

long long builtin_aleatorio(long long a, long long b) {
    long long rng = b - a + 1;
    if (rng <= 0) return a;
    return a + builtin_azar(rng);
}

/* ---- Cadenas — utilidad interna ---- */

static char* _psc_leak_str(const char* s, size_t len) {
    return (char*)_psc_gc_intern_string(s, len);
}

/* ---- Cadenas — funciones p\xC3\xBAblicas ---- */

long long builtin_longitud(const char* s) {
    return s ? (long long)strlen(s) : 0;
}

const char* builtin_mayusculas(const char* s) {
    if (!s) return "";
    size_t len = strlen(s);
    char* p = _psc_leak_str(s, len);
    for (size_t i = 0; i < len; i++) p[i] = (char)toupper((unsigned char)p[i]);
    return p;
}

const char* builtin_minusculas(const char* s) {
    if (!s) return "";
    size_t len = strlen(s);
    char* p = _psc_leak_str(s, len);
    for (size_t i = 0; i < len; i++) p[i] = (char)tolower((unsigned char)p[i]);
    return p;
}

const char* builtin_subcadena(const char* s, long long x, long long y) {
    if (!s) return _psc_leak_str("", 0);
    size_t len = strlen(s);
    size_t start = (x > 0) ? (size_t)(x - 1) : 0;
    size_t end   = (size_t)y;
    if (start >= len) return _psc_leak_str("", 0);
    if (end > len) end = len;
    if (end <= start) return _psc_leak_str("", 0);
    return _psc_leak_str(s + start, end - start);
}

const char* builtin_concatenar(const char* s1, const char* s2) {
    if (!s1) s1 = "";
    if (!s2) s2 = "";
    size_t l1 = strlen(s1), l2 = strlen(s2);
    char* p = (char*)_psc_gc_malloc(l1 + l2 + 1);
    if (!p) return "";
    memcpy(p, s1, l1);
    memcpy(p + l1, s2, l2);
    p[l1 + l2] = '\0';
    return p;
}

/* ---- Conversi\xC3\xB3n ---- */

double builtin_convertiranumero(const char* s) {
    return s ? atof(s) : 0.0;
}

const char* builtin_convertiratexto(double n) {
    char buf[64];
    snprintf(buf, sizeof(buf), "%g", n);
    return _psc_leak_str(buf, strlen(buf));
}

const char* builtin_int_to_str(long long n) {
    char buf[64];
    snprintf(buf, sizeof(buf), "%lld", n);
    return _psc_leak_str(buf, strlen(buf));
}

/* ---- Tiempo ---- */

long long builtin_horaactual(void) {
    time_t t = time(NULL);
    struct tm* lt = localtime(&t);
    return (long long)(lt->tm_hour * 10000 + lt->tm_min * 100 + lt->tm_sec);
}

long long builtin_fechaactual(void) {
    time_t t = time(NULL);
    struct tm* lt = localtime(&t);
    return (long long)((lt->tm_year + 1900) * 10000 + (lt->tm_mon + 1) * 100 + lt->tm_mday);
}

/* ---- Arreglos ---- */

long long builtin_alloc_array(long long n) {
    if (n <= 0) n = 1;
    void* p = _psc_gc_malloc((size_t)n * sizeof(long long));
    memset(p, 0, (size_t)n * sizeof(long long));
    return (long long)(size_t)p;
}

/* ---- Pantalla / Flush ---- */

int builtin_clear_screen(void) {
#ifdef _WIN32
    system("cls");
#else
    printf("\033[2J\033[H");
#endif
    fflush(stdout);
    return 0;
}

int flush_stdout(void) {
    fflush(stdout);
    return 0;
}

int builtin_sleep_secs(long long secs) {
#ifdef _WIN32
    Sleep((DWORD)(secs * 1000));
#else
    sleep((unsigned int)secs);
#endif
    return 0;
}

int builtin_sleep_millis(long long millis) {
#ifdef _WIN32
    Sleep((DWORD)millis);
#else
    usleep((useconds_t)(millis * 1000));
#endif
    return 0;
}

int builtin_wait_key(void) {
    /* Esperar que el usuario presione una tecla */
#ifdef _WIN32
    system("pause >nul");
#elif defined(__APPLE__)
    system("read -n1 -s 2>/dev/null; true");
#else
    /* Linux y otros POSIX */
    system("read -n1 -s 2>/dev/null || head -c1 2>/dev/null");
#endif
    return 0;
}
"#;
