-- Which directory the server opens for a buffer: the Gradle build's root.

local root = {}

local SETTINGS = { "settings.gradle.kts", "settings.gradle" }
local BUILD = { "build.gradle.kts", "build.gradle" }

--- The nearest directory with a Gradle settings file (the build's root,
--- not a module's); else the version-control root; else the nearest
--- directory with a build file; else the buffer's own directory.
---@param buffer integer
---@return string
function root.of(buffer)
  return vim.fs.root(buffer, SETTINGS)
    or vim.fs.root(buffer, { ".git" })
    or vim.fs.root(buffer, BUILD)
    or vim.fs.dirname(vim.api.nvim_buf_get_name(buffer))
end

return root
