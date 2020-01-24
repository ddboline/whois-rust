use whois_rust::*;

#[async_std::test]
async fn test() -> Result<(), WhoIsError> {
    let who = WhoIs::from_path("tests/data/servers.json").await?;

    let result = who.lookup(WhoIsLookupOptions::from_string("magiclen.org")?).await?;
    println!("{}", result);

    let result = who.lookup(WhoIsLookupOptions::from_string("66.42.43.17")?).await?;
    println!("{}", result);

    let result = who.lookup(WhoIsLookupOptions::from_string("fe80::5400:1ff:feaf:b71")?).await?;
    println!("{}", result);
    Ok(())
}
