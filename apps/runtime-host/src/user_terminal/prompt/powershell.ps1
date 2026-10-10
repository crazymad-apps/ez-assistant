function global:prompt {
    $savedExit = $global:LASTEXITCODE
    $current = $executionContext.SessionState.Path.CurrentLocation
    $location = $current.Path
    $root = ''
    if ($current.Provider.Name -eq 'FileSystem') { $root = [System.IO.Path]::GetPathRoot($location) }
    if ($location -eq $HOME) { $label = '~' }
    elseif ($root -and ($location.TrimEnd([char[]]'\/') -eq $root.TrimEnd([char[]]'\/'))) { $label = $root }
    else { $label = [regex]::Split($location.TrimEnd([char[]]'\/'), '[\\/]')[-1] }
    $label = [regex]::Replace($label, '[\x00-\x1f\x7f]', '?')
    $characters = [System.Globalization.StringInfo]::new($label)
    if ($characters.LengthInTextElements -gt 32) { $label = $characters.SubstringByTextElements(0, 29) + '...' }
    $global:LASTEXITCODE = $savedExit
    'PS ' + $label + ('>' * ($nestedPromptLevel + 1)) + ' '
}
