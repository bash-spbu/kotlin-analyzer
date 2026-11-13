(function_declaration
  (function_body) @function.inside) @function.around

(anonymous_function
  (function_body) @function.inside) @function.around

(lambda_literal) @function.around

(class_declaration
  (class_body) @class.inside) @class.around

(class_declaration
  (enum_class_body) @class.inside) @class.around

(object_declaration
  (class_body) @class.inside) @class.around

(line_comment)+ @comment.around

(multiline_comment) @comment.around
