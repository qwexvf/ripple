; GDScript definitions. A script file is a class, so every function in it is a
; method of that class (named by `class_name`, else by the file); the adapter
; qualifies it. Inner `class` blocks own their own functions.

(function_definition name: (name) @name) @def.method

(class_definition name: (name) @name) @def.class
(class_name_statement name: (name) @name) @def.class
(enum_definition name: (name) @name) @def.enum

; script-level state only, not a function's locals
(source (variable_statement name: (name) @name) @def.variable)
(source (export_variable_statement name: (name) @name) @def.variable)
(source (onready_variable_statement name: (name) @name) @def.variable)
(source (const_statement name: (name) @name) @def.variable)
(source (signal_statement name: (name) @name) @def.variable)
