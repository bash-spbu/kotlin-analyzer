// SYNTAX TEST "source.kotlin" "strings, numbers, operators, labels and comments"
fun expressions(input: Any?) {
    val greeting = "Hi, $name! You owe ${amounts.sumOf { it.value }} \u20AC\n"
//                 ^ punctuation.definition.string.begin.kotlin
//                      ^ punctuation.definition.template-expression.begin.kotlin
//                       ^^^^ variable.other.kotlin
//                                     ^^ punctuation.definition.template-expression.begin.kotlin
//                                       ^^^^^^^ meta.template.expression.kotlin
//                                               ^^^^^ entity.name.function.call.kotlin
//                                                     ^ punctuation.section.block.begin.kotlin
//                                                                ^^ meta.template.expression.kotlin
//                                                                   ^^^^^^ constant.character.escape.kotlin
//                                                                         ^^ constant.character.escape.kotlin
//                                                                           ^ punctuation.definition.string.end.kotlin
    val query = """
        SELECT * FROM t WHERE id = ${input} -- \n is not an escape here
//                                 ^^ punctuation.definition.template-expression.begin.kotlin
//                                          ^^^^^^^^^^^^ string.quoted.triple.kotlin
//                                             ^^ - constant.character.escape.kotlin
    """""
//  ^^^^^ string.quoted.triple.kotlin
    val newline = '\n'
//                ^^^^ string.quoted.single.kotlin
//                 ^^ constant.character.escape.kotlin

    val numbers = listOf(0xFF_FFu, 0b1010, 3.14f, .5, 1e10, 2.5e-3, 7L, 1_000)
//                       ^^^^^^^^ constant.numeric.hex.kotlin
//                                 ^^^^^^ constant.numeric.binary.kotlin
//                                         ^^^^^ constant.numeric.float.kotlin
//                                                ^^ constant.numeric.float.kotlin
//                                                    ^^^^ constant.numeric.float.kotlin
//                                                          ^^^^^^ constant.numeric.float.kotlin
//                                                                  ^^ constant.numeric.integer.kotlin
//                                                                      ^^^^^ constant.numeric.integer.kotlin
    for (index in 1..10) {}
//             ^^ keyword.operator.type.kotlin
//                ^ constant.numeric.integer.kotlin
//                 ^^ keyword.operator.range.kotlin
//                   ^^ constant.numeric.integer.kotlin
    for (index in 0..<size) {}
//                 ^^^ keyword.operator.range.kotlin

    val text = input as? String ?: return
//                   ^^^ keyword.operator.cast.kotlin
//                              ^^ keyword.operator.elvis.kotlin
//                                 ^^^^^^ keyword.control.kotlin
    if (input !is Number && input !in emptyList<Any>()) return
//            ^^^ keyword.operator.type.kotlin
//                       ^^ keyword.operator.logical.kotlin
//                                ^^^ keyword.operator.type.kotlin
//                                    ^^^^^^^^^ entity.name.function.call.kotlin
    val length = text?.length!!
//                   ^^ keyword.operator.null-safety.kotlin
//                           ^^ keyword.operator.null-safety.kotlin
    val reference = String::length
//                        ^^ keyword.operator.reference.kotlin

    outer@ for (row in rows) {
//  ^^^^^ entity.name.label.kotlin
//       ^ punctuation.definition.label.kotlin
//         ^^^ keyword.control.kotlin
        rows.forEach { if (it == null) return@forEach else break@outer }
//                                     ^^^^^^ keyword.control.kotlin
//                                           ^ punctuation.definition.label.kotlin
//                                            ^^^^^^^ entity.name.label.kotlin
//                                                         ^^^^^ keyword.control.kotlin
//                                                               ^^^^^ entity.name.label.kotlin
    }
    when (input) { null -> Unit; true, false -> Unit }
//  ^^^^ keyword.control.kotlin
//                 ^^^^ constant.language.null.kotlin
//                      ^^ keyword.operator.arrow.kotlin
//                               ^^^^ constant.language.boolean.kotlin
    val `when` = HttpStatus.NOT_FOUND
//      ^^^^^^ variable.other.definition.kotlin
//               ^^^^^^^^^^ entity.name.type.kotlin
//                          ^^^^^^^^^ variable.other.constant.kotlin
}

/* a block comment /* nested */ still a comment */ val after = 1
// <---- comment.block.kotlin
//                             ^^^^^^^^^^^^^^^^^^^ comment.block.kotlin
//                                                 ^^^ storage.type.variable.kotlin

/**
 * Pays [invoice].
 * @param invoice what to pay
// ^^^^^^ keyword.other.documentation.kotlin
//        ^^^^^^^ variable.parameter.documentation.kotlin
 */
fun pay(invoice: Invoice) = Unit // trailing
//                               ^^ punctuation.definition.comment.kotlin
//                               ^^^^^^^^^^^ comment.line.double-slash.kotlin
