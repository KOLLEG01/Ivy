# Synthetic, public test credentials. Never writes a machine certificate store.
$ErrorActionPreference='Stop'
$rsa=[Security.Cryptography.RSA]::Create(2048)
$leafKey=[Security.Cryptography.RSA]::Create(2048)
try {
  $sha=[Security.Cryptography.HashAlgorithmName]::SHA256
  $padding=[Security.Cryptography.RSASignaturePadding]::Pkcs1
  $rootRequest=[Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=Phone SIP fixture root',$rsa,$sha,$padding)
  $rootRequest.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($true,$false,0,$true))
  $rootRequest.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new([Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyCertSign -bor [Security.Cryptography.X509Certificates.X509KeyUsageFlags]::CrlSign,$true))
  $root=$rootRequest.CreateSelfSigned([DateTimeOffset]'2010-01-01Z',[DateTimeOffset]'2100-01-01Z')
  try {
    [IO.File]::WriteAllBytes("$PSScriptRoot/phone-sip-tls-root.der",$root.Export([Security.Cryptography.X509Certificates.X509ContentType]::Cert))
    [IO.File]::WriteAllBytes("$PSScriptRoot/phone-sip-tls-key.der",$leafKey.ExportPkcs8PrivateKey())
    foreach ($kind in @('valid','expired','wrong-name','self-signed')) {
      $request=[Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=Phone SIP fixture leaf',$leafKey,$sha,$padding)
      $request.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509BasicConstraintsExtension]::new($false,$false,0,$true))
      $request.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new([Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature -bor [Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyEncipherment,$true))
      $eku=[Security.Cryptography.OidCollection]::new()
      [void]$eku.Add([Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.1'))
      $request.CertificateExtensions.Add([Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new($eku,$true))
      $san=[Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new()
      if ($kind -eq 'wrong-name') { $san.AddDnsName('other.example.test') }
      else {
        $san.AddDnsName('sip.example.test')
        $san.AddDnsName('proxy.example.test')
        $san.AddIpAddress([Net.IPAddress]::Loopback)
        $san.AddIpAddress([Net.IPAddress]::IPv6Loopback)
      }
      $request.CertificateExtensions.Add($san.Build())
      $start=[DateTimeOffset]'2020-01-01Z'
      $end=[DateTimeOffset]'2090-01-01Z'
      if ($kind -eq 'expired') { $start=[DateTimeOffset]'2018-01-01Z'; $end=[DateTimeOffset]'2019-01-01Z' }
      if ($kind -eq 'self-signed') { $certificate=$request.CreateSelfSigned($start,$end) }
      else { $certificate=$request.Create($root,$start,$end,[Security.Cryptography.RandomNumberGenerator]::GetBytes(16)) }
      try { [IO.File]::WriteAllBytes("$PSScriptRoot/phone-sip-tls-$kind.der",$certificate.Export([Security.Cryptography.X509Certificates.X509ContentType]::Cert)) }
      finally { $certificate.Dispose() }
    }
  } finally { $root.Dispose() }
} finally { $rsa.Dispose(); $leafKey.Dispose() }
