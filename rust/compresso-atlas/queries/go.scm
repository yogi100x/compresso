; Declarations are anchored under `source_file`, so a match is package-level
; without walking parents; grouped specs are expanded from the declaration.
(function_declaration) @definition.function
(method_declaration) @definition.method
(source_file (type_declaration) @definition.type)
(source_file (const_declaration) @definition.const)
(source_file (var_declaration) @definition.variable)

(import_spec) @import.import
