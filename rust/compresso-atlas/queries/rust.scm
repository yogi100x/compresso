; Definitions are deliberately broad: malformed files can still yield every complete item.
(function_item) @definition.function
(struct_item) @definition.struct
(enum_item) @definition.enum
(trait_item) @definition.trait
(const_item) @definition.const
(type_item) @definition.type
(mod_item) @definition.module

; Import expansion is performed from each captured declaration so grouped uses retain order.
(use_declaration) @import.use
