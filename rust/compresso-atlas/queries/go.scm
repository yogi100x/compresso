; Package-level filtering, receiver owners, and grouped declarations are
; handled after these syntax captures.
(function_declaration) @definition.function
(method_declaration) @definition.method
(type_spec) @definition.type
(type_alias) @definition.type
(const_spec) @definition.const
(var_spec) @definition.variable

(import_spec) @import.import
