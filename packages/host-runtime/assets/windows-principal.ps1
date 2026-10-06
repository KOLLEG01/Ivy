function Resolve-IvyPrincipalSid([string]$UserId) {
  if ([string]::IsNullOrWhiteSpace($UserId)) { throw 'Missing Windows task principal.' }
  if ($UserId.StartsWith('S-')) { return [System.Security.Principal.SecurityIdentifier]::new($UserId).Value }
  return ([System.Security.Principal.NTAccount]::new($UserId).Translate([System.Security.Principal.SecurityIdentifier])).Value
}
