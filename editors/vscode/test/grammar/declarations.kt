// SYNTAX TEST "source.kotlin" "declarations, modifiers and soft keywords used as names"
package com.example.billing
// <------- keyword.other.package.kotlin
//      ^^^^^^^^^^^^^^^^^^^ entity.name.namespace.kotlin

import kotlinx.coroutines.flow.Flow as KFlow
// <------ keyword.other.import.kotlin
//     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^ entity.name.namespace.kotlin
//                                  ^^ keyword.other.as.kotlin
//                                     ^^^^^ entity.name.type.alias.kotlin
import java.util.*
//               ^ keyword.operator.wildcard.kotlin

@file:JvmName("Billing")
// <- punctuation.definition.annotation.kotlin
//   ^ punctuation.separator.annotation.kotlin
//    ^^^^^^^ storage.type.annotation.kotlin

private data class Invoice<T : Any>(val amount: Long, var note: String?) : Comparable<Invoice<T>> {
// <------- storage.modifier.kotlin
//      ^^^^ storage.modifier.kotlin
//           ^^^^^ storage.type.class.kotlin
//                 ^^^^^^^ entity.name.type.class.kotlin
//                         ^ entity.name.type.kotlin
//                             ^^^ support.class.kotlin
//                                  ^^^ storage.type.variable.kotlin
//                                      ^^^^^^ variable.other.definition.kotlin
//                                              ^^^^ support.class.kotlin
//                                                    ^^^ storage.type.variable.kotlin
//                                                        ^^^^ variable.other.definition.kotlin
//                                                                    ^ keyword.operator.nullable.kotlin
//                                                                         ^^^^^^^^^^ entity.name.type.kotlin
    override fun compareTo(other: Invoice<T>): Int = amount.compareTo(other.amount)
//  ^^^^^^^^ storage.modifier.kotlin
//           ^^^ storage.type.function.kotlin
//               ^^^^^^^^^ entity.name.function.kotlin
//                                                          ^^^^^^^^^ entity.name.function.call.kotlin
//                                                                    ^^^^^ - entity.name.function.call.kotlin

    val total: Long
        get() = amount * 2
//      ^^^ storage.type.accessor.kotlin
    var label: String = ""
        private set
//      ^^^^^^^ storage.modifier.kotlin
//              ^^^ storage.type.accessor.kotlin

    companion object {
//  ^^^^^^^^^ storage.modifier.kotlin
//            ^^^^^^ storage.type.kotlin
        const val MAX_AMOUNT = 1_000_000L
//      ^^^^^ storage.modifier.kotlin
//                ^^^^^^^^^^ variable.other.definition.kotlin
//                             ^^^^^^^^^^ constant.numeric.integer.kotlin
    }
}

fun <T> List<T>.second(): T = this[1]
// <--- storage.type.function.kotlin
//   ^ entity.name.type.kotlin
//      ^^^^ support.class.kotlin
//              ^^^^^^ entity.name.function.kotlin
//                            ^^^^ variable.language.this.kotlin

fun interface Validator { fun validate(value: String): Boolean }
// <--- storage.type.kotlin
//  ^^^^^^^^^ storage.type.class.kotlin
//            ^^^^^^^^^ entity.name.type.class.kotlin
//                            ^^^^^^^^ entity.name.function.kotlin

fun names(data: String, value: Int, where: String) {
//        ^^^^ - storage.modifier.kotlin
//                      ^^^^^ - storage.modifier.kotlin
//                                  ^^^^^ - keyword.other.where.kotlin
    val data = data.trim()
//      ^^^^ variable.other.definition.kotlin
//             ^^^^ - storage.modifier.kotlin
    val open = value > 0 && where.isNotEmpty()
//      ^^^^ variable.other.definition.kotlin
//             ^^^^^ - storage.modifier.kotlin
}

fun <T> pick(items: List<T>): T where T : Comparable<T> = items.max()
//                              ^^^^^ keyword.other.where.kotlin

class Cached(delegate: Lazy<Int>) : Lazy<Int> by delegate {
//                                            ^^ keyword.other.by.kotlin
    val cached by lazy { compute() }
//             ^^ keyword.other.by.kotlin
//                ^^^^ entity.name.function.call.kotlin
}
