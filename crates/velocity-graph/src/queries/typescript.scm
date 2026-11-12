(interface_declaration name: (type_identifier) @name) @definition.interface
(type_alias_declaration name: (type_identifier) @name) @definition.type
(enum_declaration name: (identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(method_signature name: (property_identifier) @name) @definition.method
(abstract_method_signature name: (property_identifier) @name) @definition.method
(function_signature name: (identifier) @name) @definition.function
(implements_clause (type_identifier) @name) @reference.implementation
(extends_type_clause (type_identifier) @name) @reference.implementation
((type_identifier) @name) @reference.type
(extends_clause value: (identifier) @name) @reference.implementation
