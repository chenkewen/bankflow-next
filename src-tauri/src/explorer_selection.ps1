$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$shell = New-Object -ComObject Shell.Application
$selectedWindows = @($shell.Windows() | Where-Object { [long]$_.HWND -eq $targetHwnd })
if ($selectedWindows.Count -ne 1) { throw '请在一个资源管理器窗口中选中文件后重试。' }
$items = @($selectedWindows[0].Document.SelectedItems())
if ($items.Count -eq 0) { throw '请先在资源管理器中选中文件。' }
if ($items.Count -gt 100) { throw '快捷转换一次最多选择 100 个文件。' }
$paths = @($items | ForEach-Object {
    if ($_.IsFolder -or -not $_.IsFileSystem) { throw '快捷转换仅支持本地文件，不支持文件夹。' }
    [string]$_.Path
})
ConvertTo-Json -InputObject $paths -Compress
