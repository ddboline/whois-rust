use whois_rust::{WhoIs, WhoIsError, WhoIsLookupOptions};

#[tokio::test]
async fn test() -> Result<(), WhoIsError> {
    let who = WhoIs::from_path("tests/data/servers.json").await?;

    let result = who.lookup(WhoIsLookupOptions::from_string("magiclen.org").unwrap()).await?;
    println!("{}", result);

    let result = who.lookup(WhoIsLookupOptions::from_string("66.42.43.17").unwrap()).await?;
    println!("{}", result);

    let result =
        who.lookup(WhoIsLookupOptions::from_string("fe80::5400:1ff:feaf:b71").unwrap()).await?;
    println!("{}", result);
    Ok(())
}
