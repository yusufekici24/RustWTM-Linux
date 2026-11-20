# Load only the installer's preflight functions. All process and service
# operations below are doubles. The installer entry point is never invoked.
$ErrorActionPreference = 'Stop'
$installer = Join-Path (Split-Path $PSScriptRoot -Parent) 'install.ps1'
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($installer, [ref]$null, [ref]$parseErrors)
if ($parseErrors) { throw 'Installer syntax is invalid' }
foreach ($name in 'Resolve-InstallDirectory', 'Get-InstallService', 'Close-TargetGui') {
    $definition = $ast.Find({ param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
    }, $true)
    if (-not $definition) { throw "Missing installer function $name" }
    . ([scriptblock]::Create($definition.Extent.Text))
}

function Assert-Rejected([scriptblock]$Action, [string]$ExpectedMessage) {
    try { $null = & $Action }
    catch {
        if ($_.Exception.Message -notlike $ExpectedMessage) { throw }
        return
    }
    throw "Expected refusal: $ExpectedMessage"
}

$ServiceName = 'RustWTM'
$script:registration = $null
$script:openedServices = 0
$script:processes = @()
$script:closed = [Collections.Generic.List[int]]::new()
function Get-CimInstance { param($ClassName, $Filter)
    if ($ClassName -cne 'Win32_Service' -or $Filter -cne "Name = 'RustWTM'") { throw 'Unexpected service discovery' }
    $script:registration
}
function Get-Service { param($Name)
    if ($Name -cne $ServiceName) { throw 'Wrong service selected' }
    $script:openedServices++
    [pscustomobject]@{ Status = 'Running' }
}
function Get-Process { param($Name, $ErrorAction) $script:processes }
function Stop-Process { param([Parameter(ValueFromPipeline)]$InputObject, [switch]$Force)
    process { $script:closed.Add($InputObject.Id) }
}

$dir = Join-Path $env:ProgramFiles 'RustWTM'
$portableDir = Join-Path $env:LOCALAPPDATA 'RustWTM\portable'
if ((Resolve-InstallDirectory) -ine $dir) { throw 'Unexpected machine install default' }
if ((Resolve-InstallDirectory -Portable) -ine $portableDir) { throw 'Portable install must default to the user profile' }
if ((Resolve-InstallDirectory "$dir\" -Portable) -ine $dir) { throw 'Explicit install directory was ignored' }
Assert-Rejected { Resolve-InstallDirectory ([IO.Path]::GetPathRoot($dir)) } 'Choose a RustWTM directory*'
Assert-Rejected { Resolve-InstallDirectory '\\server\share\' -Portable } 'Choose a RustWTM directory*'

$exe = Join-Path $dir 'wtm-service.exe'
if (Get-InstallService $exe) { throw 'An absent service was selected' }
if (Get-InstallService $exe -WithoutService) { throw 'NoService selected an absent service' }
$script:registration = [pscustomobject]@{ PathName = ('"' + $exe.ToUpperInvariant() + '" service') }
if ((Get-InstallService $exe).Status -ne 'Running') { throw 'Matching service was not found' }
$script:registration.PathName = '"%ProgramFiles%\RustWTM\wtm-service.exe" service'
if ((Get-InstallService $exe).Status -ne 'Running') { throw 'Expanded service path was not recognized' }
$script:registration.PathName = 'C:\RustWTM\wtm-service.exe service'
if ((Get-InstallService 'C:\RustWTM\wtm-service.exe').Status -ne 'Running') { throw 'Unquoted executable path was not recognized' }
Assert-Rejected { Get-InstallService $exe } 'The RustWTM service is registered at a different path*'
Assert-Rejected { Get-InstallService $exe -WithoutService } 'A RustWTM service is already registered*'
$script:registration.PathName = '"C:\missing-end-quote.exe service'
Assert-Rejected { Get-InstallService $exe } 'Could not identify the registered RustWTM service executable*'
if ($script:openedServices -ne 3) { throw 'A refused service reached Get-Service' }

$dir = Join-Path $env:ProgramFiles 'RustWTM [preview]'
$script:processes = @(
    [pscustomobject]@{ Id = 1; Path = "$dir\rustwtm.exe".ToUpperInvariant() },
    [pscustomobject]@{ Id = 2; Path = "$dir-sibling\rustwtm.exe" },
    [pscustomobject]@{ Id = 3; Path = "$dir\nested\rustwtm.exe" },
    [pscustomobject]@{ Id = 4; Path = $null }
)
Close-TargetGui $dir
if ($script:closed.Count -ne 1 -or $script:closed[0] -ne 1) { throw 'Installer closed programs outside the exact target' }
$script:closed.Clear()
Assert-Rejected { Close-TargetGui $dir -Portable } 'Close the RustWTM programs in the portable directory*'
$script:processes += [pscustomobject]@{ Id = 5; Path = "$dir\wtm.exe" }
Assert-Rejected { Close-TargetGui $dir } 'Close the RustWTM CLI in the installation directory*'
$script:processes = @([pscustomobject]@{ Id = 6; Path = "$dir\wtm-service.exe" })
Assert-Rejected { Close-TargetGui $dir -Portable } 'Close the RustWTM programs in the portable directory*'
Close-TargetGui "$dir-sibling" -Portable
if ($script:closed.Count -ne 0) { throw 'Portable extraction or an active CLI closed a program' }
Write-Output 'PASS: installer directory defaults, root rejection, service image ownership, NoService refusal, exact process targets, and portable no-stop behavior'
