//! SEO interceptor (spec §11): share links `/s/:postId` serve og: meta to crawlers
//! (media resolved through public IPFS gateways) and a 302 to the web app to humans.

pub mod bot_detect;
pub mod share_page;
