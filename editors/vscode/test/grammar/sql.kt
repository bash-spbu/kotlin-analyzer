// SYNTAX TEST "source.kotlin" "SQL in strings that read as SQL or are marked as SQL"
class Repository(private val jdbc: JdbcClient) {
    fun sums(id: Long) = jdbc.sql(
        """
        WITH filtered AS (SELECT code FROM $table WHERE id = ${id})
//      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ meta.embedded.block.sql
//                                          ^^^^^ variable.other.kotlin
//                                                           ^^ punctuation.definition.template-expression.begin.kotlin
        SELECT * FROM filtered
//      ^^^^^^^^^^^^^^^^^^^^^^ meta.embedded.block.sql
        """.trimIndent()
//      ^^^ punctuation.definition.string.end.kotlin
//      ^^^ - meta.embedded.block.sql
//          ^^^^^^^^^^ - meta.embedded.block.sql
    )

    fun rename() = jdbcTemplate.update("update markup set name = ?", name)
//                                      ^^^^^^^^^^^^^^^^^^^^^^^^^^ meta.embedded.line.sql
//                                                                ^ punctuation.definition.string.end.kotlin

    // language=SQL
//  ^^^^^^^^^^^^^^^ comment.line.double-slash.kotlin
    val ddl = "CREATE TABLE t (id int)"
//  ^^^ storage.type.variable.kotlin
//             ^^^^^^^^^^^^^^^^^^^^^^^ meta.embedded.line.sql
    val after = "CREATE TABLE u"
//               ^^^^^^^^^^^^^^ - meta.embedded.line.sql

    // language=SQL

    val unmarked = "CREATE TABLE v"
//                  ^^^^^^^^^^^^^^ - meta.embedded.line.sql

    val failure = "Update failed for $id"
//                 ^^^^^^^^^^^^^^^^^^^^^ - meta.embedded.line.sql
    val prompt = "select one of: a, b"
//                ^^^^^^^^^^^^^^^^^^^ - meta.embedded.line.sql
    val message = """
        Nothing to see
//      ^^^^^^^^^^^^^^ - meta.embedded.block.sql
        """
}
