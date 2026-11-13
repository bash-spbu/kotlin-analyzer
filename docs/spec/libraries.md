# Libraries

A library is one classpath entry of a source set: a compiled JAR, optionally
with a sources JAR, or the JDK.

## What is read

| source | read how | gives |
|---|---|---|
| sources JAR (`*-sources.jar`) | parsed like workspace files (Kotlin or Java) | declarations with signatures and documentation, navigable source |
| compiled JAR | the JVM sidecar | declarations with exact signatures |
| JDK | `lib/src.zip` of the project's JDK, modules `java.base`, `java.sql`, `java.net.http`, `java.logging`, `java.xml` by default | declarations and source |

When both a sources JAR and the compiled JAR exist, declarations come from
sources (they carry documentation and parameter names); the compiled JAR is
used for what sources lack. A library's declarations are computed once per
JAR content (size + modification time + path) and stored on disk; nothing is
recomputed while the JAR is unchanged.

## Sidecar protocol

`jvm/sidecar` is a small JVM program started once per server, speaking
newline-delimited JSON on stdin/stdout.

Request: `{"read": "/path/to/lib.jar"}` — reply: one line,
```json
{"jar": "/path/to/lib.jar", "classes": [ <class>, … ], "packageMembers": [ <member>, … ]}
```
or `{"jar": …, "error": "message"}`. `{"shutdown": true}` ends it.

A `<class>`:
```json
{
  "fqName": "org.example.Outer.Inner",
  "kind": "class | interface | object | companionObject | enum | annotation",
  "modifiers": ["data", "sealed", "abstract", "open", "inner", "value", "fun", "expect"],
  "visibility": "public | protected | internal | private",
  "typeParameters": [{ "name": "T", "upperBounds": [<type>] }],
  "supertypes": [<type>],
  "members": [<member>],
  "enumEntries": ["RED", "GREEN"],
  "sealedSubclasses": ["org.example.Outer.A"],
  "origin": "kotlin | java",
  "deprecated": false
}
```
A `<member>`:
```json
{
  "name": "map",
  "kind": "function | property | constructor | field",
  "static": false,
  "visibility": "public",
  "typeParameters": [ … ],
  "receiver": <type> | null,
  "parameters": [{ "name": "transform", "type": <type>, "hasDefault": false, "vararg": false }],
  "returnType": <type>,
  "modifiers": ["suspend", "inline", "operator", "infix", "const", "var", "override"],
  "deprecated": false
}
```
A `<type>`: `{"class": "kotlin.collections.List", "arguments": [<projection>], "nullable": false}`,
`{"typeParameter": "T", "nullable": true}`, or
`{"function": {"receiver": <type>|null, "parameters": [<type>], "returns": <type>, "suspend": false}, "nullable": false}`;
a `<projection>` is `{"variance": "in|out|invariant", "type": <type>}` or `"*"`.
Java types are reported as written in bytecode signatures; Kotlin maps them
(see resolution.md, *mapped types*). Class names are always fully qualified.
