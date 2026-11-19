-- kotlin-analyzer for Neovim 0.11+: finds (or downloads) the server and
-- enables it for Kotlin and Java buffers through `vim.lsp.config`.
--
--   require("kotlin_analyzer").setup({
--     cmd = nil,          -- a command to run instead of finding the binary
--     download = true,    -- download the release when there's no binary
--     java = true,        -- also serve Java buffers
--     codelens = true,    -- show the server's code lenses (run a test,
--                         -- reference counts); run one with
--                         -- vim.lsp.codelens.run()
--     init_options = { diagnostics = { disabled = { "unused-import" } } },
--     lsp = {},           -- anything else for vim.lsp.config (on_attach, …)
--   })

local release = require("kotlin_analyzer.release")
local version = require("kotlin_analyzer.version")

local kotlin_analyzer = {}

local NAME = "kotlin_analyzer"
local RELOAD_COMMAND = "kotlin-analyzer.reloadWorkspace"

local defaults = {
  cmd = nil,
  download = true,
  java = true,
  codelens = true,
  init_options = {},
  lsp = {},
}

--- Where the running configuration's binary came from.
---@type { source: "setting"|"path"|"release", command: string[] }|nil
kotlin_analyzer.server = nil

local function notify(message, level)
  vim.notify("kotlin-analyzer: " .. message, level or vim.log.levels.INFO)
end

local function enable(options, server)
  kotlin_analyzer.server = server
  local filetypes = options.java and { "kotlin", "java" } or { "kotlin" }
  local configuration = vim.tbl_deep_extend("force", options.lsp, {
    cmd = server.command,
    filetypes = filetypes,
    init_options = options.init_options,
  })
  vim.lsp.config(NAME, configuration)
  vim.lsp.enable(NAME)
end

local function clients()
  return vim.lsp.get_clients({ name = NAME })
end

local function reload_workspace()
  local running = clients()
  if #running == 0 then
    notify("not running", vim.log.levels.WARN)
    return
  end
  for _, client in ipairs(running) do
    client:request("workspace/executeCommand", { command = RELOAD_COMMAND, arguments = {} }, function(error)
      if error then
        notify("reload failed: " .. error.message, vim.log.levels.ERROR)
      end
    end)
  end
end

--- The commands the server's code lenses carry, which the client runs.
local function register_lens_commands()
  vim.lsp.commands["kotlin-analyzer.runTests"] = function(command)
    local run = command.arguments[1]
    require("kotlin_analyzer.gradle").run_tests(vim.uri_to_fname(run.uri), run.filter)
  end
  vim.lsp.commands["kotlin-analyzer.showReferences"] = function(command, context)
    local client = vim.lsp.get_client_by_id(context.client_id)
    local encoding = client and client.offset_encoding or "utf-16"
    local items = vim.lsp.util.locations_to_items(command.arguments[3], encoding)
    vim.fn.setqflist({}, " ", { title = "References", items = items })
    vim.cmd("botright copen")
  end
end

--- Shows the code lenses in the buffers the server attaches to.
local function show_code_lenses()
  vim.api.nvim_create_autocmd("LspAttach", {
    group = vim.api.nvim_create_augroup("kotlin_analyzer_codelens", { clear = true }),
    callback = function(event)
      local client = vim.lsp.get_client_by_id(event.data.client_id)
      if client and client.name == NAME then
        vim.lsp.codelens.enable(true, { bufnr = event.buf })
      end
    end,
  })
end

local function install_release(options)
  notify("downloading the server " .. version)
  release.install(version, function(error)
    if error then
      notify(error, vim.log.levels.ERROR)
      return
    end
    notify("installed " .. release.binary(version))
    enable(options, { source = "release", command = { release.binary(version) } })
  end)
end

local function create_commands(options)
  vim.api.nvim_create_user_command("KotlinAnalyzerReload", reload_workspace, {
    desc = "Export the Gradle model again if the build changed, and reload every file",
  })
  require("kotlin_analyzer.gradle").create_command()
  vim.api.nvim_create_user_command("KotlinAnalyzerInstall", function()
    vim.fn.delete(release.binary(version))
    install_release(options)
  end, { desc = "Download this plugin's server release again" })
end

---@param user_options table|nil
function kotlin_analyzer.setup(user_options)
  local options = vim.tbl_deep_extend("force", defaults, user_options or {})
  create_commands(options)
  register_lens_commands()
  if options.codelens then
    show_code_lenses()
  end
  if options.cmd then
    enable(options, { source = "setting", command = options.cmd })
  elseif vim.fn.executable("kotlin-analyzer") == 1 then
    enable(options, { source = "path", command = { vim.fn.exepath("kotlin-analyzer") } })
  elseif vim.uv.fs_stat(release.binary(version)) then
    enable(options, { source = "release", command = { release.binary(version) } })
  elseif options.download then
    install_release(options)
  else
    notify("no kotlin-analyzer on the PATH; install it or pass `cmd`", vim.log.levels.ERROR)
  end
end

return kotlin_analyzer
