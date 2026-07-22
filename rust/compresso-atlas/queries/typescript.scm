; Module-level filtering and export normalization happen after these syntax captures.
(function_declaration) @definition.function
(class_declaration) @definition.class
(method_definition) @definition.method
(interface_declaration) @definition.interface
(enum_declaration) @definition.enum
(type_alias_declaration) @definition.type
(variable_declarator) @definition.variable

(import_statement
  source: (string) @import.import)

(export_statement
  source: (string) @import.export_from)
