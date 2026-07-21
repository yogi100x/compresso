; Broad syntax captures are filtered to stable module-level symbols in Rust code.
(function_declaration) @definition.function
(class_declaration) @definition.class
(method_definition) @definition.method
(variable_declarator) @definition.variable
(assignment_expression) @definition.module_export

(import_statement
  source: (string) @import.import)

; The normalizer accepts only identifier `require` with one string argument.
(call_expression
  function: (identifier)
  arguments: (arguments)) @import.require
