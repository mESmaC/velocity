; Definitions. A function inside an impl or trait becomes a method in code.
(struct_item name: (type_identifier) @name) @definition.class
(enum_item name: (type_identifier) @name) @definition.class
(union_item name: (type_identifier) @name) @definition.class
(type_item name: (type_identifier) @name) @definition.type
(trait_item name: (type_identifier) @name) @definition.interface
(mod_item name: (identifier) @name) @definition.module
(macro_definition name: (identifier) @name) @definition.macro
(function_item name: (identifier) @name) @definition.function
(function_signature_item name: (identifier) @name) @definition.function
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.constant

; Impl blocks name the type their methods belong to, and the trait they implement.
(impl_item trait: (_) @container.trait type: (_) @container.name) @container
(impl_item !trait type: (_) @container.name) @container

; Calls: plain, path-qualified (Type::new, module::function) and method calls.
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (scoped_identifier path: (_) @qualifier name: (identifier) @name)) @reference.call
(call_expression function: (field_expression value: (self) @qualifier field: (field_identifier) @name)) @reference.method
(call_expression function: (field_expression field: (field_identifier) @name)) @reference.method
(call_expression function: (generic_function function: (identifier) @name)) @reference.call
(call_expression function: (generic_function function: (scoped_identifier path: (_) @qualifier name: (identifier) @name))) @reference.call
(call_expression function: (generic_function function: (field_expression field: (field_identifier) @name))) @reference.method
(macro_invocation macro: (identifier) @name) @reference.call
(struct_expression name: (type_identifier) @name) @reference.call
(struct_expression name: (scoped_type_identifier path: (_) @qualifier name: (type_identifier) @name)) @reference.call

; Type uses.
((type_identifier) @name) @reference.type

; Imports: the whole use tree, expanded in code.
(use_declaration argument: (_) @import.path) @import
