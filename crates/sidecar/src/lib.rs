//! Reads compiled classes through the JVM helper in `jvm/sidecar`: one
//! request per JAR, answered with the declarations it contains — fully
//! qualified types, parameters with defaults and varargs, supertypes.
