-- kotlin-analyzer's defaults for `vim.lsp.config` (Neovim 0.11+). Enough on
-- its own when `kotlin-analyzer` is on the PATH:
--
--   vim.lsp.enable("kotlin_analyzer")
--
-- `require("kotlin_analyzer").setup()` adds downloading the release and the
-- commands on top.

return {
  cmd = { "kotlin-analyzer" },
  filetypes = { "kotlin", "java" },
  root_dir = function(buffer, on_dir)
    on_dir(require("kotlin_analyzer.root").of(buffer))
  end,
  init_options = {
    diagnostics = { disabled = {}, experimental = false },
    gradleExport = true,
  },
}
