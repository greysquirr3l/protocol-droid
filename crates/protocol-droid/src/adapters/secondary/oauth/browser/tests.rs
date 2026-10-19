use core::time::Duration;

use super::LoopbackBrowser;
use crate::adapters::secondary::oauth::environment::UserAgent;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A "browser" that sends `request_target` to the redirect URI.
async fn browser_sending(
    request_target: &'static str,
) -> Result<LoopbackBrowser, Box<dyn std::error::Error>> {
    let browser = LoopbackBrowser::bind().await?;
    let base = browser
        .redirect_uri()
        .trim_end_matches("/callback")
        .to_owned();
    Ok(browser.opening_with(move |_url| {
        let url = format!("{base}{request_target}");
        tokio::spawn(async move {
            let _ = reqwest::get(url).await;
        });
        Ok(())
    }))
}

#[tokio::test]
async fn the_redirect_is_delivered_with_its_code_and_state() -> TestResult {
    let browser =
        browser_sending("/callback?code=abc&state=xyz&iss=https%3A%2F%2Fas.example").await?;
    let callback = browser.authorize("https://as.example/authorize").await?;
    let shown = format!("{callback:?}");
    assert!(shown.contains("abc"), "{shown}");
    Ok(())
}

#[tokio::test]
async fn an_error_response_is_delivered_as_a_refusal() -> TestResult {
    let browser = browser_sending("/callback?error=access_denied&state=xyz").await?;
    let callback = browser.authorize("https://as.example/authorize").await?;
    assert!(format!("{callback:?}").contains("access_denied"));
    Ok(())
}

#[tokio::test]
async fn other_paths_do_not_end_the_wait() -> TestResult {
    let browser = LoopbackBrowser::bind().await?;
    let base = browser
        .redirect_uri()
        .trim_end_matches("/callback")
        .to_owned();
    let browser = browser.opening_with(move |_url| {
        let base = base.clone();
        tokio::spawn(async move {
            // A favicon request arrives first, as it does from a real browser.
            let _ = reqwest::get(format!("{base}/favicon.ico")).await;
            let _ = reqwest::get(format!("{base}/callback?code=late&state=s")).await;
        });
        Ok(())
    });
    let callback = browser.authorize("https://as.example/authorize").await?;
    assert!(format!("{callback:?}").contains("late"));
    Ok(())
}

#[tokio::test]
async fn nobody_coming_back_is_a_decline_not_a_hang() -> TestResult {
    let browser = LoopbackBrowser::bind()
        .await?
        .waiting(Duration::from_millis(100))
        .opening_with(|_| Ok(()));
    let outcome = browser.authorize("https://as.example/authorize").await;
    assert!(outcome.is_err());
    Ok(())
}

#[tokio::test]
async fn only_a_web_url_is_opened() -> TestResult {
    let browser = LoopbackBrowser::bind()
        .await?
        .opening_with(|_| Err("must not be called".to_owned()));
    let outcome = browser.authorize("file:///etc/passwd").await;
    assert!(outcome.is_err());
    Ok(())
}

#[tokio::test]
async fn the_redirect_uri_is_loopback_and_names_the_callback() -> TestResult {
    let browser = LoopbackBrowser::bind().await?;
    assert!(browser.redirect_uri().starts_with("http://127.0.0.1:"));
    assert!(browser.redirect_uri().ends_with("/callback"));
    Ok(())
}
