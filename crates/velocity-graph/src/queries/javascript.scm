; Shared by JavaScript and TypeScript (TypeScript adds typescript.scm).
(method_definition name: (property_identifier) @name) @definition.method
(class_declaration name: (_) @name) @definition.class
(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)])) @definition.function
(variable_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)])) @definition.function
(program (lexical_declaration (variable_declarator name: (identifier) @name)) @definition.constant)
(program (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name)) @definition.constant))

(call_expression function: (identifier) @name) @reference.call
(call_expression function: (member_expression object: [(identifier) (this)] @qualifier property: (property_identifier) @name)) @reference.method
(call_expression function: (member_expression property: (property_identifier) @name)) @reference.method
(new_expression constructor: (identifier) @name) @reference.call

(import_statement (import_clause (named_imports (import_specifier name: (identifier) @import.name))) source: (string (string_fragment) @import.source)) @import
(import_statement (import_clause (identifier) @import.name) source: (string (string_fragment) @import.source)) @import
(import_statement (import_clause (namespace_import (identifier) @import.name)) source: (string (string_fragment) @import.source)) @import
(export_statement (export_clause (export_specifier name: (identifier) @import.name)) source: (string (string_fragment) @import.source)) @import
