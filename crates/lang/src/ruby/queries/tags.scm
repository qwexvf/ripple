; Ruby Tier-0 definition captures. A top-level `def` and a `def` inside a class
; both parse as `method`; they are captured uniformly here (Ruby support is
; import-level, so member qualification is not needed yet).
(method
  name: (identifier) @name) @def.function

(singleton_method
  name: (identifier) @name) @def.function

(class
  name: (constant) @name) @def.class

(module
  name: (constant) @name) @def.class
