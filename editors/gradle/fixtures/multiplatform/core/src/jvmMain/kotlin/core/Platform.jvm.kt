package core

actual fun platformName(): String = System.getProperty("java.vendor")
