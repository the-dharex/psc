# PSC — Compilador PSeInt nativo

**psc** es un compilador para el lenguaje [PSeInt](https://pseint.sourceforge.net/) escrito en Rust. Soporta compilación **JIT** (ejecución inmediata en memoria) y generación de **ejecutables nativos** (AOT) para la plataforma de destino.

## Características

- **Compilación JIT** — Compila y ejecuta programas PSeInt al instante usando Cranelift.
- **Compilación AOT** — Genera ejecutables nativos standalone (`.exe` / ELF) enlazando con un runtime C embebido.
- **Optimizaciones multinivel** — Nivel 0 (sin optimización), nivel 1 (AST: plegado de constantes, eliminación de código muerto, reducción de fuerza, propagación), nivel 2 (AST + Cranelift).
- **Análisis semántico** — Verificación de tipos, compatibilidad Entero/Real/Logico/Cadena, validación de paso por referencia, verificación de retorno en todos los caminos.
- **Reportes de error detallados** — Mensajes en español con indicación de línea y columna mediante [ariadne](https://crates.io/crates/ariadne).
- **Funciones recursivas** — Soporte completo para `Funcion`/`SubAlgoritmo` con parámetros por valor y por referencia.
- **Arreglos multidimensionales** — `Dimension` con acceso por índice multidimensional.
- **Funciones nativas** — Matemáticas (`RC`, `Sen`, `Cos`, `Tan`, `Abs`, `Ln`, `Exp`, etc.), cadenas (`Longitud`, `Mayusculas`, `Subcadena`, `Concatenar`), conversión (`ConvertirANumero`, `ConvertirATexto`), aleatorio (`Azar`, `Aleatorio`), tiempo (`HoraActual`, `FechaActual`).

## Requisitos

- **Rust** (edición 2024)
- Para compilación AOT: un compilador C (`gcc`, `cc`, o MSVC `cl.exe`)

## Instalación

```bash
git clone <repo>
cd psc
cargo build --release
```

El ejecutable queda en `target/release/psc` (o `psc.exe` en Windows).

## Uso

### Modo JIT (compilar y ejecutar)

```bash
psc -i programa.psc
```

### Generar ejecutable nativo

```bash
psc -i programa.psc -o programa
```

Esto genera `programa.exe` (Windows) o `programa` (Linux/macOS).

### Opciones

| Opción | Descripción |
|---|---|
| `-i, --input <ARCHIVO>` | Archivo `.psc` de entrada (obligatorio) |
| `-o, --output <RUTA>` | Generar ejecutable nativo en la ruta indicada |
| `-O, --opt-level <0\|1\|2>` | Nivel de optimización (default: `2`) |
| `--stats` | Mostrar tiempos de compilación y estadísticas |
| `--no-run` | Solo compilar, no ejecutar (modo JIT) |
| `-V, --version` | Mostrar versión |

## Ejemplos

### Suma básica

```
Algoritmo Suma
    Escribir "Ingrese el primer numero:"
    Leer A
    Escribir "Ingrese el segundo numero:"
    Leer B
    C <- A + B
    Escribir "El resultado es: ", C
FinAlgoritmo
```

```bash
psc -i ejemplos/Suma.psc
```

### Función recursiva

```
Funcion resultado <- Potencia (base, exponente)
    Si exponente = 0 Entonces
        resultado <- 1
    SiNo
        resultado <- base * Potencia(base, exponente - 1)
    FinSi
FinFuncion

Algoritmo Principal
    Escribir "Ingrese Base"
    Leer base
    Escribir "Ingrese Exponente"
    Leer exponente
    Escribir "El resultado es ", Potencia(base, exponente)
FinAlgoritmo
```

### Compilar a ejecutable nativo con estadísticas

```bash
psc -i programa.psc -o programa --stats
```

```
╔══════════════════════════════════════╗
║  Compilación AOT (ejecutable nativo) ║
╠══════════════════════════════════════╣
║  Tokens:              20             ║
║  Funciones:            0             ║
║  Sentencias main:      6             ║
╠──────────────────────────────────────╣
║  Léxico:       71.200µs              ║
║  Parsing:       1.335ms              ║
║  Semántico:    76.900µs              ║
║  Optimizer:    15.900µs              ║
║  Codegen:       2.265ms              ║
║  Enlazado:       1.757s              ║
║  Total:          1.761s              ║
╚══════════════════════════════════════╝
```

## Sintaxis soportada

| Instrucción | Ejemplo |
|---|---|
| Variables | `Definir x Como Entero` |
| Asignación | `x <- 5` o `x = 5` |
| Lectura / Escritura | `Leer x` · `Escribir "Hola", x` |
| Escribir sin salto | `Escribir Sin Saltar "texto"` |
| Condicional | `Si ... Entonces ... SiNo ... FinSi` |
| Mientras | `Mientras ... Hacer ... FinMientras` |
| Repetir | `Repetir ... Hasta Que ...` |
| Para | `Para i <- 1 Hasta 10 Con Paso 1 Hacer ... FinPara` |
| Segun (Switch) | `Segun x Hacer ... De Otro Modo ... FinSegun` |
| Funciones | `Funcion ret <- Nombre(params) ... FinFuncion` |
| Sub-algoritmos | `SubAlgoritmo Nombre(params) ... FinSubAlgoritmo` |
| Arreglos | `Dimension arr[10]` · `arr[i] <- valor` |
| Esperar | `Esperar 2 Segundos` · `Esperar Tecla` |
| Borrar Pantalla | `Borrar Pantalla` |

## Tipos de dato

| Tipo | Descripción |
|---|---|
| `Entero` | Número entero de 64 bits |
| `Real` | Número de punto flotante de 64 bits |
| `Logico` | Verdadero / Falso |
| `Cadena` | Cadena de texto (puntero C) |

## Arquitectura

```
Código PSeInt (.psc)
       │
       ▼
   ┌────────┐
   │  Lexer │  logos 0.14
   └────┬───┘
        ▼
   ┌────────┐
   │ Parser │  chumsky 0.9
   └────┬───┘
        ▼
   ┌────────┐
   │  AST   │
   └────┬───┘
        ▼
   ┌────────┐
   │  Sema  │  Análisis semántico + tabla de símbolos
   └────┬───┘
        ▼
   ┌────────────┐
   │ Optimizer  │  Plegado de constantes, DCE, reducción de fuerza
   └────┬───────┘
        ▼
   ┌────────────┐
   │  Codegen   │  Cranelift 0.108
   └────┬───────┘
        │
   ┌────┴────┐
   │         │
   ▼         ▼
  JIT       AOT
(memoria)  (.obj → enlazar con runtime C → ejecutable nativo)
```

## Licencia

MIT
