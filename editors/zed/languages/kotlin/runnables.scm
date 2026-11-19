; Test functions and classes, run by the tasks tagged `kotlin-test-*` in
; tasks.json (Gradle's test task with `--tests`). A test function is one
; annotated `@Test`, `@ParameterizedTest`, `@RepeatedTest`, `@TestFactory`
; or `@TestTemplate` (JUnit 4 and 5, kotlin.test, TestNG), in a top-level
; class or a class nested in one. The captures are the tasks' variables
; (`$ZED_CUSTOM_kotlin_class_name`, …).

; A test function in a top-level class.
(source_file
  (package_header (identifier) @kotlin_package_name)?
  (class_declaration
    (type_identifier) @kotlin_class_name
    (class_body
      (function_declaration
        (modifiers
          (annotation
            [
              (user_type (type_identifier) @_annotation .)
              (constructor_invocation (user_type (type_identifier) @_annotation .))
            ]))
        (simple_identifier) @run @kotlin_method_name) @_))
  (#match? @_annotation "^(Test|ParameterizedTest|RepeatedTest|TestFactory|TestTemplate)$")
  (#set! tag kotlin-test-method))

; A test function in a nested class.
(source_file
  (package_header (identifier) @kotlin_package_name)?
  (class_declaration
    (type_identifier) @kotlin_outer_class_name
    (class_body
      (class_declaration
        (type_identifier) @kotlin_class_name
        (class_body
          (function_declaration
            (modifiers
              (annotation
                [
                  (user_type (type_identifier) @_annotation .)
                  (constructor_invocation (user_type (type_identifier) @_annotation .))
                ]))
            (simple_identifier) @run @kotlin_method_name) @_))))
  (#match? @_annotation "^(Test|ParameterizedTest|RepeatedTest|TestFactory|TestTemplate)$")
  (#set! tag kotlin-test-nested-method))

; A top-level class holding a test function.
(source_file
  (package_header (identifier) @kotlin_package_name)?
  (class_declaration
    (type_identifier) @run @kotlin_class_name
    (class_body
      (function_declaration
        (modifiers
          (annotation
            [
              (user_type (type_identifier) @_annotation .)
              (constructor_invocation (user_type (type_identifier) @_annotation .))
            ])))) @_)
  (#match? @_annotation "^(Test|ParameterizedTest|RepeatedTest|TestFactory|TestTemplate)$")
  (#set! tag kotlin-test-class))

; A nested class holding a test function.
(source_file
  (package_header (identifier) @kotlin_package_name)?
  (class_declaration
    (type_identifier) @kotlin_outer_class_name
    (class_body
      (class_declaration
        (type_identifier) @run @kotlin_class_name
        (class_body
          (function_declaration
            (modifiers
              (annotation
                [
                  (user_type (type_identifier) @_annotation .)
                  (constructor_invocation (user_type (type_identifier) @_annotation .))
                ])))) @_)))
  (#match? @_annotation "^(Test|ParameterizedTest|RepeatedTest|TestFactory|TestTemplate)$")
  (#set! tag kotlin-test-nested-class))
