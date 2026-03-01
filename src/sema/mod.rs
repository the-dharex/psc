
use std::collections::HashMap;
use crate::ast::{Program, Statement, Expression, Type, BinaryOp, UnaryOp};

#[derive(Debug, Clone)]
pub struct SymbolTable {
    scopes: Vec<Scope>,
    /// Tipos de elementos de arreglos: nombre_arreglo -> tipo_elemento
    array_elem_types: HashMap<String, Type>,
}

#[derive(Debug, Clone)]
struct Scope {
    symbols: HashMap<String, Symbol>,
}

#[derive(Debug, Clone)]
pub enum Symbol {
    Variable { ty: Type },
    Function { params: Vec<(Type, bool)>, ret: Option<Type> },
}

impl SymbolTable {
    pub fn new() -> Self {
        let mut symbols = HashMap::new();
        
        // Registrar todas las funciones nativas de PSeInt
        // Matemáticas: Real -> Real
        for name in &["rc", "raiz", "abs", "ln", "exp", "sen", "cos", "tan", "asen", "acos", "atan"] {
            symbols.insert(name.to_string(), Symbol::Function { params: vec![(Type::Real, false)], ret: Some(Type::Real) });
        }
        // Matemáticas: Real -> Entero
        for name in &["trunc", "redon"] {
            symbols.insert(name.to_string(), Symbol::Function { params: vec![(Type::Real, false)], ret: Some(Type::Integer) });
        }
        // Aleatorio
        symbols.insert("azar".to_string(), Symbol::Function { params: vec![(Type::Integer, false)], ret: Some(Type::Integer) });
        symbols.insert("aleatorio".to_string(), Symbol::Function { params: vec![(Type::Integer, false), (Type::Integer, false)], ret: Some(Type::Integer) });
        // Funciones de cadena
        symbols.insert("longitud".to_string(), Symbol::Function { params: vec![(Type::String, false)], ret: Some(Type::Integer) });
        symbols.insert("mayusculas".to_string(), Symbol::Function { params: vec![(Type::String, false)], ret: Some(Type::String) });
        symbols.insert("minusculas".to_string(), Symbol::Function { params: vec![(Type::String, false)], ret: Some(Type::String) });
        symbols.insert("subcadena".to_string(), Symbol::Function { params: vec![(Type::String, false), (Type::Integer, false), (Type::Integer, false)], ret: Some(Type::String) });
        symbols.insert("concatenar".to_string(), Symbol::Function { params: vec![(Type::String, false), (Type::String, false)], ret: Some(Type::String) });
        // Conversión
        symbols.insert("convertiranumero".to_string(), Symbol::Function { params: vec![(Type::String, false)], ret: Some(Type::Real) });
        symbols.insert("convertiratexto".to_string(), Symbol::Function { params: vec![(Type::Real, false)], ret: Some(Type::String) });
        // Tiempo
        symbols.insert("horaactual".to_string(), Symbol::Function { params: vec![], ret: Some(Type::Integer) });
        symbols.insert("fechaactual".to_string(), Symbol::Function { params: vec![], ret: Some(Type::Integer) });

        SymbolTable {
            scopes: vec![Scope { symbols }],
            array_elem_types: HashMap::new(),
        }
    }

    pub fn enter_scope(&mut self) {
        self.scopes.push(Scope { symbols: HashMap::new() });
    }

    pub fn exit_scope(&mut self) {
        self.scopes.pop();
    }

    pub fn insert(&mut self, name: String, symbol: Symbol) -> Result<(), String> {
        if let Some(scope) = self.scopes.last_mut() {
            if scope.symbols.contains_key(&name) {
                return Err(format!("Symbol '{}' already defined in this scope", name));
            }
            scope.symbols.insert(name, symbol);
            Ok(())
        } else {
            Err("No scope to insert symbol into".to_string())
        }
    }

    pub fn lookup(&self, name: &str) -> Option<&Symbol> {
        for scope in self.scopes.iter().rev() {
            if let Some(symbol) = scope.symbols.get(name) {
                return Some(symbol);
            }
        }
        None
    }

    pub fn set_array_elem_type(&mut self, name: &str, ty: Type) {
        self.array_elem_types.insert(name.to_string(), ty);
    }

    pub fn get_array_elem_type(&self, name: &str) -> Option<&Type> {
        self.array_elem_types.get(name)
    }
}

pub fn analyze(program: &Program) -> Result<SymbolTable, Vec<String>> {
    let mut sym_table = SymbolTable::new();
    let mut errors = Vec::new();

    // 1. Registrar funciones globales
    for func in &program.functions {
        let param_types: Vec<(Type, bool)> = func.params.iter().map(|(_, ty, by_ref)| (ty.clone(), *by_ref)).collect();
        let symbol = Symbol::Function {
            params: param_types,
            ret: func.return_type.clone(),
        };
        if let Err(e) = sym_table.insert(func.name.clone(), symbol.clone()) {
            errors.push(e);
        }
        // También registrar versión en minúsculas para búsqueda sin distinción de mayúsculas
        let lower = func.name.to_lowercase();
        if lower != func.name {
            let _ = sym_table.insert(lower, symbol);
        }
    }

    // 2. Analizar cuerpo principal
    sym_table.enter_scope();
    analyze_block(&program.main_body, &mut sym_table, &mut errors);
    sym_table.exit_scope();

    // 3. Analizar cuerpos de funciones
    for func in &program.functions {
        sym_table.enter_scope();
        // Agregar parámetros al ámbito
        for (name, ty, _) in &func.params {
             if let Err(e) = sym_table.insert(name.clone(), Symbol::Variable { ty: ty.clone() }) {
                 errors.push(format!("Función '{}': {}", func.name, e));
             }
        }
        // Agregar variable de retorno al ámbito si existe
        if let Some(ref ret_var) = func.return_var {
            let ret_ty = func.return_type.clone().unwrap_or(Type::Integer);
            if let Err(e) = sym_table.insert(ret_var.clone(), Symbol::Variable { ty: ret_ty }) {
                errors.push(format!("Función '{}': {}", func.name, e));
            }
        }
        
        analyze_block(&func.body, &mut sym_table, &mut errors);

        // Verificar retorno: si la función tiene tipo de retorno no Void y return_var,
        // verificar que la variable de retorno se asigne en todos los caminos
        if let Some(ref ret_var) = func.return_var {
            if let Some(ref ret_ty) = func.return_type {
                if *ret_ty != Type::Void {
                    if !block_assigns_var_any(&func.body, ret_var) {
                        errors.push(format!(
                            "Función '{}': tiene tipo de retorno {} pero la variable '{}' nunca se asigna",
                            func.name, ret_ty, ret_var
                        ));
                    } else if !block_assigns_var_all_paths(&func.body, ret_var) {
                        errors.push(format!(
                            "Advertencia: Función '{}': la variable de retorno '{}' no se asigna en todos los caminos",
                            func.name, ret_var
                        ));
                    }
                }
            }
        }

        sym_table.exit_scope();
    }

    if errors.is_empty() {
        Ok(sym_table)
    } else {
        Err(errors)
    }
}

/// Verifica si un bloque asigna `var_name` en **todos** los caminos de ejecución.
fn block_assigns_var_all_paths(stmts: &[Statement], var_name: &str) -> bool {
    for stmt in stmts {
        match stmt {
            Statement::Assign { target, .. } if target == var_name => return true,
            Statement::If { then_branch, else_branch, .. } => {
                // Asignado en todos los caminos solo si ambas ramas existen y ambas asignan
                if let Some(eb) = else_branch {
                    if block_assigns_var_all_paths(then_branch, var_name)
                        && block_assigns_var_all_paths(eb, var_name)
                    {
                        return true;
                    }
                }
                // Si no hay else, o solo una rama asigna, no garantiza todos los caminos
            }
            Statement::Match { cases, default, .. } => {
                // Asignado en todos los caminos si todos los cases + default asignan
                if let Some(d) = default {
                    if !cases.is_empty()
                        && cases.iter().all(|(_, body)| block_assigns_var_all_paths(body, var_name))
                        && block_assigns_var_all_paths(d, var_name)
                    {
                        return true;
                    }
                }
            }
            // Bucles no garantizan ejecución (puede ser 0 iteraciones)
            _ => {}
        }
    }
    false
}

/// Verifica si un bloque (o cualquier bloque anidado) contiene al menos una asignación a `var_name`.
fn block_assigns_var_any(stmts: &[Statement], var_name: &str) -> bool {
    for stmt in stmts {
        match stmt {
            Statement::Assign { target, .. } if target == var_name => return true,
            Statement::If { then_branch, else_branch, .. } => {
                if block_assigns_var_any(then_branch, var_name) { return true; }
                if let Some(eb) = else_branch {
                    if block_assigns_var_any(eb, var_name) { return true; }
                }
            }
            Statement::While { body, .. } | Statement::Repeat { body, .. } => {
                if block_assigns_var_any(body, var_name) { return true; }
            }
            Statement::For { body, .. } => {
                if block_assigns_var_any(body, var_name) { return true; }
            }
            Statement::Match { cases, default, .. } => {
                for (_, body) in cases {
                    if block_assigns_var_any(body, var_name) { return true; }
                }
                if let Some(d) = default {
                    if block_assigns_var_any(d, var_name) { return true; }
                }
            }
            _ => {}
        }
    }
    false
}

fn analyze_block(stmts: &[Statement], sym_table: &mut SymbolTable, errors: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            Statement::Define { vars, ty } => {
                for var in vars {
                    // Omitir si ya existe (parámetro u otra definición previa)
                    if sym_table.lookup(var).is_none() {
                        if let Err(e) = sym_table.insert(var.clone(), Symbol::Variable { ty: ty.clone() }) {
                            errors.push(e);
                        }
                    }
                }
            }
            Statement::Dimension { name, sizes } => {
                // Validar que las expresiones de tamaño sean numéricas
                for size_expr in sizes {
                    check_expr_type(size_expr, sym_table, errors);
                }
                // Registrar variable de arreglo (almacenada como puntero Entero)
                if sym_table.lookup(name).is_none() {
                    if let Err(e) = sym_table.insert(name.clone(), Symbol::Variable { ty: Type::Integer }) {
                        errors.push(e);
                    }
                }
            }
            Statement::Assign { target, value } => {
                let value_ty = check_expr_type(value, sym_table, errors);
                
                if let Some(val_ty) = value_ty {
                    if let Some(Symbol::Variable { ty: target_ty }) = sym_table.lookup(target) {
                        if !types_compatible(target_ty, &val_ty) {
                                errors.push(format!("Tipos incompatibles en asignación a '{}': esperado {}, encontrado {}", target, target_ty, val_ty));
                        }
                    } else {
                        // Declaración implícita
                        if let Err(e) = sym_table.insert(target.clone(), Symbol::Variable { ty: val_ty }) {
                             errors.push(e);
                        }
                    }
                }
            }
            Statement::IndexAssign { array, indices, value } => {
                // Validar que el arreglo exista
                if sym_table.lookup(array).is_none() {
                    errors.push(format!("Arreglo '{}' no definido para asignación por índice", array));
                }
                // Validar expresiones de índice
                for idx in indices {
                    if let Some(idx_ty) = check_expr_type(idx, sym_table, errors) {
                        if idx_ty != Type::Integer && idx_ty != Type::Real {
                            errors.push(format!("Índice de arreglo debe ser numérico, encontrado {}", idx_ty));
                        }
                    }
                }
                // Inferir y rastrear tipo de elemento
                if let Some(val_ty) = check_expr_type(value, sym_table, errors) {
                    sym_table.set_array_elem_type(array, val_ty);
                }
            }
            Statement::If { condition, then_branch, else_branch } => {
                 if let Some(ty) = check_expr_type(condition, sym_table, errors) {
                     if ty != Type::Boolean && ty != Type::Integer {
                          errors.push(format!("La condición del Si debe ser Logico, encontrado {}", ty));
                     }
                 }
                 analyze_block(then_branch, sym_table, errors);
                 if let Some(else_branch) = else_branch {
                     analyze_block(else_branch, sym_table, errors);
                 }
            }
            Statement::While { condition, body } => {
                if let Some(ty) = check_expr_type(condition, sym_table, errors) {
                     if ty != Type::Boolean && ty != Type::Integer {
                          errors.push(format!("La condición del Mientras debe ser Logico, encontrado {}", ty));
                     }
                 }
                 analyze_block(body, sym_table, errors);
            }
            Statement::Repeat { body, until } => {
                 analyze_block(body, sym_table, errors);
                 if let Some(ty) = check_expr_type(until, sym_table, errors) {
                     if ty != Type::Boolean && ty != Type::Integer {
                          errors.push(format!("La condición del Repetir Hasta Que debe ser Logico, encontrado {}", ty));
                     }
                 }
            }
            Statement::For { var, start, end, step, body } => {
                // Validar que inicio/fin/paso sean numéricos
                check_expr_type(start, sym_table, errors);
                check_expr_type(end, sym_table, errors);
                if let Some(step_expr) = step {
                    check_expr_type(step_expr, sym_table, errors);
                }
                if let Some(Symbol::Variable { ty }) = sym_table.lookup(var) {
                    if *ty != Type::Integer && *ty != Type::Real {
                        errors.push(format!("La variable del Para '{}' debe ser numérica", var));
                    }
                } else {
                    // Declaración implícita — PSeInt permite variables de Para sin declarar
                    if let Err(e) = sym_table.insert(var.clone(), Symbol::Variable { ty: Type::Integer }) {
                        errors.push(e);
                    }
                }
                analyze_block(body, sym_table, errors);
            }
            Statement::Match { expression, cases, default } => {
                check_expr_type(expression, sym_table, errors);
                for (case_expr, case_body) in cases {
                    check_expr_type(case_expr, sym_table, errors);
                    analyze_block(case_body, sym_table, errors);
                }
                if let Some(default_body) = default {
                    analyze_block(default_body, sym_table, errors);
                }
            }
            Statement::Read(targets) => {
                for target in targets {
                    match target {
                        Expression::Variable(var) => {
                            if sym_table.lookup(var).is_none() {
                                if let Err(e) = sym_table.insert(var.clone(), Symbol::Variable { ty: Type::Integer }) {
                                    errors.push(e);
                                }
                            }
                        }
                        Expression::Index { array, indices } => {
                            if sym_table.lookup(array).is_none() {
                                errors.push(format!("Arreglo '{}' no definido para Leer", array));
                            }
                            for idx in indices {
                                check_expr_type(idx, sym_table, errors);
                            }
                        }
                        _ => {
                            errors.push("Destino inválido para Leer".to_string());
                        }
                    }
                }
            }
            Statement::Write(exprs, _) => {
                for expr in exprs {
                    check_expr_type(expr, sym_table, errors);
                }
            }
            Statement::Call { function, args } => {
                let func_lower = function.to_lowercase();
                if let Some(Symbol::Function { params, .. }) = sym_table.lookup(&func_lower).cloned() {
                    if args.len() != params.len() {
                        errors.push(format!(
                            "Llamada a '{}': esperados {} argumentos, encontrados {}",
                            function, params.len(), args.len()
                        ));
                    } else {
                        for (i, (arg, (expected_ty, by_ref))) in args.iter().zip(params.iter()).enumerate() {
                            if *by_ref {
                                // Paso por referencia: el argumento debe ser una variable
                                if !matches!(arg, Expression::Variable(_) | Expression::Index { .. }) {
                                    errors.push(format!(
                                        "Llamada a '{}': argumento {} es por referencia y debe ser una variable",
                                        function, i + 1
                                    ));
                                }
                            }
                            if let Some(arg_ty) = check_expr_type(arg, sym_table, errors) {
                                if !types_compatible(expected_ty, &arg_ty) {
                                    errors.push(format!(
                                        "Llamada a '{}': argumento {} esperado {}, encontrado {}",
                                        function, i + 1, expected_ty, arg_ty
                                    ));
                                }
                            }
                        }
                    }
                } else if sym_table.lookup(function).is_none() && sym_table.lookup(&func_lower).is_none() {
                    errors.push(format!("Función o procedimiento '{}' no definido", function));
                }
            }
            Statement::Wait { duration, .. } => {
                if let Some(ty) = check_expr_type(duration, sym_table, errors) {
                    if ty != Type::Integer && ty != Type::Real {
                        errors.push(format!("La duración de Esperar debe ser numérica, encontrado {}", ty));
                    }
                }
            }
            _ => {} 
        }
    }
}

/// Verifica tipos de una expresión, reportando errores específicos. Retorna el tipo inferido o None.
fn check_expr_type(expr: &Expression, sym_table: &SymbolTable, errors: &mut Vec<String>) -> Option<Type> {
    match expr {
        Expression::Literal(lit) => match lit {
            crate::ast::Literal::Integer(_) => Some(Type::Integer),
            crate::ast::Literal::Real(_) => Some(Type::Real),
            crate::ast::Literal::Boolean(_) => Some(Type::Boolean),
            crate::ast::Literal::String(_) => Some(Type::String),
        },
        Expression::Variable(name) => {
            if let Some(Symbol::Variable { ty }) = sym_table.lookup(name) {
                Some(ty.clone())
            } else if sym_table.lookup(name).is_some() {
                // Es una función, no una variable
                errors.push(format!("'{}' es una función, no una variable", name));
                None
            } else {
                errors.push(format!("Variable '{}' no definida", name));
                None
            }
        }
        Expression::Binary { left, op, right } => {
            let left_ty = check_expr_type(left, sym_table, errors);
            let right_ty = check_expr_type(right, sym_table, errors);
            
            let (left_ty, right_ty) = match (left_ty, right_ty) {
                (Some(l), Some(r)) => (l, r),
                _ => return None, // Errores ya reportados por llamadas recursivas
            };
            
            match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod | BinaryOp::Power => {
                    // Concatenación de cadenas con +
                    if matches!(op, BinaryOp::Add) && (left_ty == Type::String || right_ty == Type::String) {
                        return Some(Type::String);
                    }
                    if (left_ty == Type::Integer || left_ty == Type::Real) && (right_ty == Type::Integer || right_ty == Type::Real) {
                        if left_ty == Type::Real || right_ty == Type::Real || matches!(op, BinaryOp::Div | BinaryOp::Power) {
                            Some(Type::Real)
                        } else {
                            Some(Type::Integer)
                        }
                    } else {
                        errors.push(format!("Operación aritmética {:?} entre tipos incompatibles: {} y {}", op, left_ty, right_ty));
                        None
                    }
                }
                BinaryOp::Eq | BinaryOp::Ne => {
                    if types_compatible(&left_ty, &right_ty) || types_compatible(&right_ty, &left_ty) {
                        Some(Type::Boolean)
                    } else {
                        errors.push(format!("Comparación {:?} entre tipos incompatibles: {} y {}", op, left_ty, right_ty));
                        None
                    }
                }
                BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                    if (left_ty == Type::String && right_ty != Type::String) || (left_ty != Type::String && right_ty == Type::String) {
                        errors.push(format!("No se puede comparar {} con {}", left_ty, right_ty));
                        None
                    } else if (left_ty == Type::Integer || left_ty == Type::Real || left_ty == Type::String) && 
                              (right_ty == Type::Integer || right_ty == Type::Real || right_ty == Type::String) {
                        Some(Type::Boolean)
                    } else {
                        errors.push(format!("Comparación {:?} entre tipos incompatibles: {} y {}", op, left_ty, right_ty));
                        None
                    }
                }
                BinaryOp::And | BinaryOp::Or => {
                    if left_ty != Type::Boolean {
                        errors.push(format!("Operando izquierdo de {:?} debe ser Logico, encontrado {}", op, left_ty));
                    }
                    if right_ty != Type::Boolean {
                        errors.push(format!("Operando derecho de {:?} debe ser Logico, encontrado {}", op, right_ty));
                    }
                    if left_ty == Type::Boolean && right_ty == Type::Boolean {
                        Some(Type::Boolean)
                    } else {
                        None
                    }
                }
            }
        }
        Expression::Unary { op, expr: inner } => {
            let expr_ty = check_expr_type(inner, sym_table, errors)?;
            match op {
                UnaryOp::Neg => {
                    if expr_ty == Type::Integer || expr_ty == Type::Real {
                        Some(expr_ty)
                    } else {
                        errors.push(format!("Negación aplicada a tipo no numérico: {}", expr_ty));
                        None
                    }
                }
                UnaryOp::Not => {
                    if expr_ty == Type::Boolean {
                         Some(Type::Boolean)
                    } else {
                         errors.push(format!("Operador NO aplicado a tipo no logico: {}", expr_ty));
                         None
                    }
                }
            }
        }
        Expression::Call { function, args } => {
            let func_lower = function.to_lowercase();
            if let Some(Symbol::Function { params, ret }) = sym_table.lookup(&func_lower).cloned() {
                // Verificar cantidad de argumentos
                if args.len() != params.len() {
                    errors.push(format!(
                        "Llamada a '{}': esperados {} argumentos, encontrados {}",
                        function, params.len(), args.len()
                    ));
                } else {
                    // Verificar tipos de argumentos y paso por referencia
                    for (i, (arg, (expected_ty, by_ref))) in args.iter().zip(params.iter()).enumerate() {
                        if *by_ref {
                            if !matches!(arg, Expression::Variable(_) | Expression::Index { .. }) {
                                errors.push(format!(
                                    "Llamada a '{}': argumento {} es por referencia y debe ser una variable",
                                    function, i + 1
                                ));
                            }
                        }
                        if let Some(arg_ty) = check_expr_type(arg, sym_table, errors) {
                            if !types_compatible(expected_ty, &arg_ty) {
                                errors.push(format!(
                                    "Llamada a '{}': argumento {} esperado {}, encontrado {}",
                                    function, i + 1, expected_ty, arg_ty
                                ));
                            }
                        }
                    }
                }
                ret
            } else {
                errors.push(format!("Función '{}' no definida", function));
                None
            }
        }
        Expression::Index { array, indices } => {
            // Validar que el arreglo exista
            if sym_table.lookup(array).is_none() {
                errors.push(format!("Arreglo '{}' no definido", array));
            }
            // Validar que los índices sean numéricos
            for idx in indices {
                if let Some(idx_ty) = check_expr_type(idx, sym_table, errors) {
                    if idx_ty != Type::Integer && idx_ty != Type::Real {
                        errors.push(format!("Índice de arreglo debe ser numérico, encontrado {}", idx_ty));
                    }
                }
            }
            // Retornar tipo de elemento rastreado, por defecto Entero
            Some(sym_table.get_array_elem_type(array).cloned().unwrap_or(Type::Integer))
        }
    }
}

fn types_compatible(target: &Type, value: &Type) -> bool {
    if target == value {
        return true;
    }
    // Promover automáticamente Entero a Real
    if *target == Type::Real && *value == Type::Integer {
        return true;
    }
    // Destino Entero acepta Real (PSeInt es flexible)
    if *target == Type::Integer && *value == Type::Real {
        return true;
    }
    // Logico <-> Entero son intercambiables (PSeInt trata 0=Falso, <>0=Verdadero)
    if (*target == Type::Boolean && *value == Type::Integer)
        || (*target == Type::Integer && *value == Type::Boolean)
    {
        return true;
    }
    false
}
