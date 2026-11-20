// SYNTAX TEST "source.kotlin" "JSON and YAML in strings a comment marks"
class Fixtures {
    // language=JSON
//  ^^^^^^^^^^^^^^^^ comment.line.double-slash.kotlin
    val body = """{"id": $id}"""
//  ^^^ storage.type.variable.kotlin
//                ^^^^^^^^ meta.embedded.block.json
//                ^ meta.structure.dictionary.json
//                        ^^ variable.other.kotlin
//                             ^^^ punctuation.definition.string.end.kotlin
    val after = """{"id": 1}"""
//                 ^^^^^^^^^ - meta.embedded.block.json

    fun send() = client.post(
        /* language=json */
        """
        {"ok": true}
//      ^^^^^^^^^^^^ meta.embedded.block.json
        """.trimIndent()
//          ^^^^^^^^^^ - meta.embedded.block.json
    )

    // language=YAML
    val config = """
        server:
//      ^^^^^^^ meta.embedded.block.yaml
          port: 8080
//        ^^^^^^^^^^ meta.embedded.block.yaml
        """.trimIndent()
//      ^^^ - meta.embedded.block.yaml

    // language=JSON
    val escaped = "{\"id\": 1}"
//                 ^^^^^^^^^^^ - meta.embedded.block.json
    val unmarked = """{"id": 1}"""
//                    ^^^^^^^^^ - meta.embedded.block.json
}
