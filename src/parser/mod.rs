use chumsky::prelude::*;
use crate::lexer::Token;
use crate::ast::{Program, Statement, Expression, BinaryOp, UnaryOp, Literal, Type, Function};

pub fn parser() -> impl Parser<Token, Program, Error = Simple<Token>> {
    let ident = select! { Token::Ident(ident) => ident };

    let val = select! {
        Token::Int(n) => Literal::Integer(n),
        Token::Float(s) => Literal::Real(s.parse().unwrap()),
        Token::String(s) => Literal::String(s),
        Token::Verdadero => Literal::Boolean(true),
        Token::Falso => Literal::Boolean(false),
    }
    .map(Expression::Literal);

    let expr = recursive(|expr| {
        // Analizar identificador seguido opcionalmente de (args) o [índices]
        let call = ident
            .then(
                // Llamada a función: ident(arg1, arg2, ...)
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .delimited_by(just(Token::LParen), just(Token::RParen))
                    .map(|args| (true, args)) // true = llamada
                .or(
                    // Array index: ident[idx1, idx2, ...]
                    expr.clone()
                        .separated_by(just(Token::Comma))
                        .delimited_by(just(Token::LBracket), just(Token::RBracket))
                        .map(|indices| (false, indices)) // false = índice
                )
                .or_not()
            )
            .map(|(name, access)| {
                match access {
                    Some((true, arguments)) => Expression::Call { function: name, args: arguments },
                    Some((false, indices)) => Expression::Index { array: name, indices },
                    None => Expression::Variable(name),
                }
            });

        let atom = val
            .or(call)
            .or(expr.clone().delimited_by(just(Token::LParen), just(Token::RParen)));

        let unary = just(Token::Minus).to(UnaryOp::Neg)
            .or(just(Token::Not).to(UnaryOp::Not))
            .repeated()
            .then(atom)
            .foldr(|op, rhs| Expression::Unary { op, expr: Box::new(rhs) });

        let product = unary.clone()
            .then(just(Token::Star).to(BinaryOp::Mul)
                .or(just(Token::Slash).to(BinaryOp::Div))
                .or(just(Token::Mod).to(BinaryOp::Mod))
                .or(just(Token::Caret).to(BinaryOp::Power))
                .then(unary)
                .repeated())
            .foldl(|lhs, (op, rhs)| Expression::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            });

        let sum = product.clone()
            .then(just(Token::Plus).to(BinaryOp::Add)
                .or(just(Token::Minus).to(BinaryOp::Sub))
                .then(product)
                .repeated())
            .foldl(|lhs, (op, rhs)| Expression::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            });
        
        let comparison = sum.clone()
            .then(just(Token::Eq).to(BinaryOp::Eq)
                .or(just(Token::Ne).to(BinaryOp::Ne))
                .or(just(Token::Lt).to(BinaryOp::Lt))
                .or(just(Token::Le).to(BinaryOp::Le))
                .or(just(Token::Gt).to(BinaryOp::Gt))
                .or(just(Token::Ge).to(BinaryOp::Ge))
                .then(sum)
                .repeated())
            .foldl(|lhs, (op, rhs)| Expression::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            });
        
        let logic = comparison.clone()
            .then(just(Token::And).to(BinaryOp::And)
                .or(just(Token::Or).to(BinaryOp::Or))
                .then(comparison)
                .repeated())
            .foldl(|lhs, (op, rhs)| Expression::Binary {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            });

        logic
    });

    // Types
    let type_parser = select! {
        Token::Entero => Type::Integer,
        Token::Real => Type::Real,
        Token::Logico => Type::Boolean,
        Token::Caracter => Type::String,
        Token::Texto => Type::String, 
    };

    let stmt = recursive(|stmt| {
        let define = just(Token::Definir)
            .ignore_then(ident.separated_by(just(Token::Comma)))
            .then_ignore(just(Token::Como))
            .then(type_parser)
            .then_ignore(just(Token::Semi).or_not())
            .map(|(vars, ty)| Statement::Define { vars, ty });

        // Dimensionar arr[N] o arr[N, M] — soporta delimitadores [] y ()
        let dimension = just(Token::Dimension)
            .ignore_then(ident)
            .then(
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .delimited_by(just(Token::LBracket), just(Token::RBracket))
                .or(
                    expr.clone()
                        .separated_by(just(Token::Comma))
                        .delimited_by(just(Token::LParen), just(Token::RParen))
                )
            )
            .then_ignore(just(Token::Semi).or_not())
            .map(|(name, sizes)| Statement::Dimension { name, sizes });

        // arr[i] <- valor  (asignación indexada, debe intentarse ANTES de la asignación normal)
        let index_assign = ident
            .then(
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .delimited_by(just(Token::LBracket), just(Token::RBracket))
                .or(
                    expr.clone()
                        .separated_by(just(Token::Comma))
                        .delimited_by(just(Token::LParen), just(Token::RParen))
                )
            )
            .then_ignore(just(Token::Assign).or(just(Token::Eq)))
            .then(expr.clone())
            .then_ignore(just(Token::Semi).or_not())
            .map(|((array, indices), value)| Statement::IndexAssign { array, indices, value });

        let assign = ident
            .then_ignore(just(Token::Assign).or(just(Token::Eq))) // <- o := o =
            .then(expr.clone())
            .then_ignore(just(Token::Semi).or_not())
            .map(|(target, value)| Statement::Assign { target, value });

        let read_target = ident
            .then(
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .delimited_by(just(Token::LBracket), just(Token::RBracket))
                    .map(Some)
                .or_not()
            )
            .map(|(name, indices)| match indices.flatten() {
                Some(idx) => Expression::Index { array: name, indices: idx },
                None => Expression::Variable(name),
            });

        let read = just(Token::Leer)
            .ignore_then(read_target.separated_by(just(Token::Comma)))
            .then_ignore(just(Token::Semi).or_not())
            .map(Statement::Read);

        let write = just(Token::Escribir)
            .ignore_then(just(Token::SinSaltar).or_not())
            .then(expr.clone().separated_by(just(Token::Comma)))
            .then(just(Token::SinSaltar).or_not())
            .then_ignore(just(Token::Semi).or_not())
            .map(|((sin_saltar_before, exprs), sin_saltar_after)| {
                let has_newline = sin_saltar_before.is_none() && sin_saltar_after.is_none();
                Statement::Write(exprs, has_newline)
            });

        let if_stmt = just(Token::Si)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::Entonces))
            .then(stmt.clone().repeated())
            .then(just(Token::Sino).ignore_then(stmt.clone().repeated()).or_not())
            .then_ignore(just(Token::FinSi))
            .map(|((condition, then_branch), else_branch)| Statement::If {
                condition,
                then_branch,
                else_branch,
            });

        let while_stmt = just(Token::Mientras)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::Hacer))
            .then(stmt.clone().repeated())
            .then_ignore(just(Token::FinMientras))
            .map(|(condition, body)| Statement::While { condition, body });

        let repeat_stmt = just(Token::Repetir)
            .ignore_then(stmt.clone().repeated())
            .then_ignore(just(Token::Hasta))
            .then_ignore(just(Token::Que))
            .then(expr.clone())
            .then_ignore(just(Token::Semi).or_not())
            .map(|(body, until)| Statement::Repeat { body, until });

        let for_stmt = just(Token::Para)
            .ignore_then(ident)
            .then_ignore(just(Token::Assign).or(just(Token::Eq))) // <- o =
            .then(expr.clone())
            .then_ignore(just(Token::Hasta))
            .then(expr.clone())
            .then(just(Token::Paso).ignore_then(expr.clone()).or_not()) // Paso opcional; ConPaso mapeado a Paso
            .then_ignore(just(Token::Hacer))
            .then(stmt.clone().repeated())
            .then_ignore(just(Token::FinPara))
            .map(|((((var, start), end), step), body)| Statement::For {
                var,
                start,
                end,
                step,
                body,
            });
            
        let match_stmt = just(Token::Segun)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::Hacer))
            .then(
                expr.clone()
                    .then_ignore(just(Token::Colon))
                    .then(stmt.clone().repeated())
                    .repeated()
            )
            .then(
                just(Token::Sino) 
                    .ignore_then(just(Token::Colon).or_not())
                    .ignore_then(stmt.clone().repeated())
                    .or_not()
            )
            .then_ignore(just(Token::FinSegun))
            .map(|((expression, cases), default)| Statement::Match {
                expression,
                cases,
                default,
            });

        let clear_screen = just(Token::BorrarPantalla)
            .then_ignore(just(Token::Semi).or_not())
            .map(|_| Statement::ClearScreen);

        let wait_key_stmt = just(Token::Esperar)
            .ignore_then(just(Token::Tecla))
            .then_ignore(just(Token::Semi).or_not())
            .map(|_| Statement::WaitKey);

        let wait_stmt = just(Token::Esperar)
            .ignore_then(expr.clone())
            .then(
                just(Token::Milisegundo).to(true)
                .or(just(Token::Segundo).to(false))
            )
            .then_ignore(just(Token::Semi).or_not())
            .map(|(duration, milliseconds)| Statement::Wait { duration, milliseconds });

        let call_stmt = ident
            .then(
                expr.clone()
                    .separated_by(just(Token::Comma))
                    .delimited_by(just(Token::LParen), just(Token::RParen))
                    .or_not()
            )
            .then_ignore(just(Token::Semi).or_not())
            .map(|(function, args)| Statement::Call {
                function,
                args: args.unwrap_or_default(),
            });

        define
            .or(dimension)
            .or(clear_screen)
            .or(wait_key_stmt)
            .or(wait_stmt)
            .or(index_assign)
            .or(assign)
            .or(read)
            .or(write)
            .or(if_stmt)
            .or(while_stmt)
            .or(repeat_stmt)
            .or(for_stmt)
            .or(match_stmt)
            .or(call_stmt)
    });


    let function = just(Token::Funcion).or(just(Token::Funcion)) // Mapear ambos al token Funcion
        .ignore_then(
             ident.then(just(Token::Assign).ignore_then(ident).or_not())
        )
        .then(
            just(Token::LParen)
            .ignore_then(
                ident
                .then(
                    just(Token::PorReferencia).to(true)
                    .or(just(Token::PorValor).to(false))
                    .or_not()
                )
                .then(just(Token::Como).ignore_then(type_parser).or_not())
                .map(|((name, by_ref), ty): ((String, Option<bool>), Option<Type>)| (name, ty.unwrap_or(Type::Void), by_ref.unwrap_or(false)))
                .separated_by(just(Token::Comma))
            )
            .then_ignore(just(Token::RParen))
            .or_not()
        )
        .then(stmt.clone().repeated())
        .then_ignore(just(Token::FinFuncion).or(just(Token::FinFuncion)))
        .map(|(((name_part, ret_part), args), body): (((String, Option<String>), Option<Vec<(String, Type, bool)>>), Vec<Statement>)| {
            let (name, return_var) = match ret_part {
                Some(func_name) => (func_name, Some(name_part)),
                None => (name_part, None),
            };
            
            // Inferir tipos de parámetros no especificados desde Definir en el cuerpo
            let params = args.unwrap_or_default().into_iter().map(|(pname, ty, by_ref)| {
                if ty == Type::Void {
                    let inferred = body.iter().find_map(|s| {
                        if let Statement::Define { vars, ty: def_ty } = s {
                            if vars.iter().any(|v| v.eq_ignore_ascii_case(&pname)) {
                                Some(def_ty.clone())
                            } else { None }
                        } else { None }
                    });
                    (pname, inferred.unwrap_or(Type::Integer), by_ref)
                } else {
                    (pname, ty, by_ref)
                }
            }).collect::<Vec<_>>();

            // Inferir tipo de retorno desde Definir en el cuerpo
            let return_type = if let Some(ref rv) = return_var {
                let inferred = body.iter().find_map(|s| {
                    if let Statement::Define { vars, ty } = s {
                        if vars.iter().any(|v| v.eq_ignore_ascii_case(rv)) {
                            Some(ty.clone())
                        } else { None }
                    } else { None }
                });
                Some(inferred.unwrap_or(Type::Integer))
            } else {
                Some(Type::Void)
            };

            Function {
                name,
                params,
                return_type,
                return_var,
                body,
            }
        });

    let process = just(Token::Proceso)
        .ignore_then(ident)
        .then(stmt.clone().repeated())
        .then_ignore(just(Token::FinProceso))
        .map(|(name, main_body)| (name, main_body));

    function.clone().repeated().then(process).then(function.repeated())
        .map(|((pre_funcs, (name, main_body)), post_funcs)| {
            let mut functions: Vec<Function> = pre_funcs;
            functions.extend(post_funcs);
            Program {
                name,
                functions,
                main_body,
            }
        })
        .then_ignore(end())
}
