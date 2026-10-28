use serde::{Deserialize, Serialize};

use crate::chain::ChainId;
use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Supported token contracts and mints.
    ///
    /// Each chain's tokens sit together in chain order, and the wallet lists them as
    /// declared: other tokens first, then real-world assets with the dollar coins before
    /// gold, each group by market capitalization.
    pub enum TokenId {
        LinkErc20 => "link_erc20",
        WbtcErc20 => "wbtc_erc20",
        WethErc20 => "weth_erc20",
        UniErc20 => "uni_erc20",
        ShibErc20 => "shib_erc20",
        EnaErc20 => "ena_erc20",
        AaveErc20 => "aave_erc20",
        OndoErc20 => "ondo_erc20",
        PepeErc20 => "pepe_erc20",
        ArbErc20 => "arb_erc20",
        UsdtErc20 => "usdt_erc20",
        UsdcErc20 => "usdc_erc20",
        UsdsErc20 => "usds_erc20",
        DaiErc20 => "dai_erc20",
        UsdgErc20 => "usdg_erc20",
        PyusdErc20 => "pyusd_erc20",
        XautErc20 => "xaut_erc20",
        PaxgErc20 => "paxg_erc20",
        LinkArbitrum => "link_arbitrum",
        WethArbitrum => "weth_arbitrum",
        EnaArbitrum => "ena_arbitrum",
        ArbArbitrum => "arb_arbitrum",
        UsdtArbitrum => "usdt_arbitrum",
        UsdcArbitrum => "usdc_arbitrum",
        UsdsArbitrum => "usds_arbitrum",
        UsdgArbitrum => "usdg_arbitrum",
        PyusdArbitrum => "pyusd_arbitrum",
        XautArbitrum => "xaut_arbitrum",
        LinkBase => "link_base",
        WethBase => "weth_base",
        EnaBase => "ena_base",
        UsdcBase => "usdc_base",
        UsdsBase => "usds_base",
        LinkOptimism => "link_optimism",
        WethOptimism => "weth_optimism",
        EnaOptimism => "ena_optimism",
        OpOptimism => "op_optimism",
        UsdtOptimism => "usdt_optimism",
        UsdcOptimism => "usdc_optimism",
        UsdsOptimism => "usds_optimism",
        LinkBep20 => "link_bep20",
        WethBep20 => "weth_bep20",
        EnaBep20 => "ena_bep20",
        UsdtBep20 => "usdt_bep20",
        UsdcBep20 => "usdc_bep20",
        XautBep20 => "xaut_bep20",
        LinkPolygon => "link_polygon",
        WethPolygon => "weth_polygon",
        UsdtPolygon => "usdt_polygon",
        UsdcPolygon => "usdc_polygon",
        PyusdPolygon => "pyusd_polygon",
        XautPolygon => "xaut_polygon",
        LinkAvalanche => "link_avalanche",
        WethAvalanche => "weth_avalanche",
        UsdtAvalanche => "usdt_avalanche",
        UsdcAvalanche => "usdc_avalanche",
        UsdsAvalanche => "usds_avalanche",
        XautAvalanche => "xaut_avalanche",
        LinkSpl => "link_spl",
        WbtcSpl => "wbtc_spl",
        EnaSpl => "ena_spl",
        PumpSpl => "pump_spl",
        JupSpl => "jup_spl",
        UsdtSpl => "usdt_spl",
        UsdcSpl => "usdc_spl",
        UsdsSpl => "usds_spl",
        UsdgSpl => "usdg_spl",
        PyusdSpl => "pyusd_spl",
        XautSpl => "xaut_spl",
        PaxgSpl => "paxg_spl",
        WbtcTrc20 => "wbtc_trc20",
        UsdtTrc20 => "usdt_trc20",
        EnaJetton => "ena_jetton",
        UsdtJetton => "usdt_jetton",
        XautJetton => "xaut_jetton",
    }
}

impl TokenId {
    pub const fn chain(self) -> ChainId {
        match self {
            Self::LinkErc20
            | Self::WbtcErc20
            | Self::WethErc20
            | Self::UniErc20
            | Self::ShibErc20
            | Self::EnaErc20
            | Self::AaveErc20
            | Self::OndoErc20
            | Self::PepeErc20
            | Self::ArbErc20
            | Self::UsdtErc20
            | Self::UsdcErc20
            | Self::UsdsErc20
            | Self::DaiErc20
            | Self::UsdgErc20
            | Self::PyusdErc20
            | Self::XautErc20
            | Self::PaxgErc20 => ChainId::Ethereum,
            Self::LinkArbitrum
            | Self::WethArbitrum
            | Self::EnaArbitrum
            | Self::ArbArbitrum
            | Self::UsdtArbitrum
            | Self::UsdcArbitrum
            | Self::UsdsArbitrum
            | Self::UsdgArbitrum
            | Self::PyusdArbitrum
            | Self::XautArbitrum => ChainId::Arbitrum,
            Self::LinkBase | Self::WethBase | Self::EnaBase | Self::UsdcBase | Self::UsdsBase => {
                ChainId::Base
            }
            Self::LinkOptimism
            | Self::WethOptimism
            | Self::EnaOptimism
            | Self::OpOptimism
            | Self::UsdtOptimism
            | Self::UsdcOptimism
            | Self::UsdsOptimism => ChainId::Optimism,
            Self::LinkBep20
            | Self::WethBep20
            | Self::EnaBep20
            | Self::UsdtBep20
            | Self::UsdcBep20
            | Self::XautBep20 => ChainId::Bsc,
            Self::LinkPolygon
            | Self::WethPolygon
            | Self::UsdtPolygon
            | Self::UsdcPolygon
            | Self::PyusdPolygon
            | Self::XautPolygon => ChainId::Polygon,
            Self::LinkAvalanche
            | Self::WethAvalanche
            | Self::UsdtAvalanche
            | Self::UsdcAvalanche
            | Self::UsdsAvalanche
            | Self::XautAvalanche => ChainId::Avalanche,
            Self::LinkSpl
            | Self::WbtcSpl
            | Self::EnaSpl
            | Self::PumpSpl
            | Self::JupSpl
            | Self::UsdtSpl
            | Self::UsdcSpl
            | Self::UsdsSpl
            | Self::UsdgSpl
            | Self::PyusdSpl
            | Self::XautSpl
            | Self::PaxgSpl => ChainId::Solana,
            Self::WbtcTrc20 | Self::UsdtTrc20 => ChainId::Tron,
            Self::EnaJetton | Self::UsdtJetton | Self::XautJetton => ChainId::Ton,
        }
    }

    /// Tether's omnichain `USDT0` and `XAUt0` and bridged copies such as Avalanche's
    /// `LINK.e` take the symbol of the coin they stand for. The ether Binance issues on BNB
    /// Smart Chain as `ETH` reads as WETH, like every other network's.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::LinkErc20
            | Self::LinkArbitrum
            | Self::LinkBase
            | Self::LinkOptimism
            | Self::LinkBep20
            | Self::LinkPolygon
            | Self::LinkAvalanche
            | Self::LinkSpl => "LINK",
            Self::WbtcErc20 | Self::WbtcSpl | Self::WbtcTrc20 => "WBTC",
            Self::WethErc20
            | Self::WethArbitrum
            | Self::WethBase
            | Self::WethOptimism
            | Self::WethBep20
            | Self::WethPolygon
            | Self::WethAvalanche => "WETH",
            Self::UniErc20 => "UNI",
            Self::ShibErc20 => "SHIB",
            Self::EnaErc20
            | Self::EnaArbitrum
            | Self::EnaBase
            | Self::EnaOptimism
            | Self::EnaBep20
            | Self::EnaSpl
            | Self::EnaJetton => "ENA",
            Self::AaveErc20 => "AAVE",
            Self::OndoErc20 => "ONDO",
            Self::PepeErc20 => "PEPE",
            Self::ArbErc20 | Self::ArbArbitrum => "ARB",
            Self::OpOptimism => "OP",
            Self::PumpSpl => "PUMP",
            Self::JupSpl => "JUP",
            Self::UsdtErc20
            | Self::UsdtArbitrum
            | Self::UsdtOptimism
            | Self::UsdtBep20
            | Self::UsdtPolygon
            | Self::UsdtAvalanche
            | Self::UsdtSpl
            | Self::UsdtTrc20
            | Self::UsdtJetton => "USDT",
            Self::UsdcErc20
            | Self::UsdcArbitrum
            | Self::UsdcBase
            | Self::UsdcOptimism
            | Self::UsdcBep20
            | Self::UsdcPolygon
            | Self::UsdcAvalanche
            | Self::UsdcSpl => "USDC",
            Self::UsdsErc20
            | Self::UsdsArbitrum
            | Self::UsdsBase
            | Self::UsdsOptimism
            | Self::UsdsAvalanche
            | Self::UsdsSpl => "USDS",
            Self::DaiErc20 => "DAI",
            Self::UsdgErc20 | Self::UsdgArbitrum | Self::UsdgSpl => "USDG",
            Self::PyusdErc20 | Self::PyusdArbitrum | Self::PyusdPolygon | Self::PyusdSpl => "PYUSD",
            Self::XautErc20
            | Self::XautArbitrum
            | Self::XautBep20
            | Self::XautPolygon
            | Self::XautAvalanche
            | Self::XautSpl
            | Self::XautJetton => "XAUT",
            Self::PaxgErc20 | Self::PaxgSpl => "PAXG",
        }
    }

    /// The same on every chain.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::LinkErc20
            | Self::LinkArbitrum
            | Self::LinkBase
            | Self::LinkOptimism
            | Self::LinkBep20
            | Self::LinkPolygon
            | Self::LinkAvalanche
            | Self::LinkSpl => "Chainlink",
            Self::WbtcErc20 | Self::WbtcSpl | Self::WbtcTrc20 => "Wrapped Bitcoin",
            Self::WethErc20
            | Self::WethArbitrum
            | Self::WethBase
            | Self::WethOptimism
            | Self::WethBep20
            | Self::WethPolygon
            | Self::WethAvalanche => "Wrapped Ether",
            Self::UniErc20 => "Uniswap",
            Self::ShibErc20 => "Shiba Inu",
            Self::EnaErc20
            | Self::EnaArbitrum
            | Self::EnaBase
            | Self::EnaOptimism
            | Self::EnaBep20
            | Self::EnaSpl
            | Self::EnaJetton => "Ethena",
            Self::AaveErc20 => "Aave",
            Self::OndoErc20 => "Ondo",
            Self::PepeErc20 => "Pepe",
            Self::ArbErc20 | Self::ArbArbitrum => "Arbitrum",
            Self::OpOptimism => "Optimism",
            Self::PumpSpl => "Pump.fun",
            Self::JupSpl => "Jupiter",
            Self::UsdtErc20
            | Self::UsdtArbitrum
            | Self::UsdtOptimism
            | Self::UsdtBep20
            | Self::UsdtPolygon
            | Self::UsdtAvalanche
            | Self::UsdtSpl
            | Self::UsdtTrc20
            | Self::UsdtJetton => "Tether USD",
            Self::UsdcErc20
            | Self::UsdcArbitrum
            | Self::UsdcBase
            | Self::UsdcOptimism
            | Self::UsdcBep20
            | Self::UsdcPolygon
            | Self::UsdcAvalanche
            | Self::UsdcSpl => "USD Coin",
            Self::UsdsErc20
            | Self::UsdsArbitrum
            | Self::UsdsBase
            | Self::UsdsOptimism
            | Self::UsdsAvalanche
            | Self::UsdsSpl => "USDS",
            Self::DaiErc20 => "Dai",
            Self::UsdgErc20 | Self::UsdgArbitrum | Self::UsdgSpl => "Global Dollar",
            Self::PyusdErc20 | Self::PyusdArbitrum | Self::PyusdPolygon | Self::PyusdSpl => {
                "PayPal USD"
            }
            Self::XautErc20
            | Self::XautArbitrum
            | Self::XautBep20
            | Self::XautPolygon
            | Self::XautAvalanche
            | Self::XautSpl
            | Self::XautJetton => "Tether Gold",
            Self::PaxgErc20 | Self::PaxgSpl => "PAX Gold",
        }
    }

    /// A dollar coin or a gold token.
    pub(crate) const fn is_real_world_asset(self) -> bool {
        match self {
            Self::UsdtErc20
            | Self::UsdcErc20
            | Self::UsdsErc20
            | Self::DaiErc20
            | Self::UsdgErc20
            | Self::PyusdErc20
            | Self::XautErc20
            | Self::PaxgErc20
            | Self::UsdtArbitrum
            | Self::UsdcArbitrum
            | Self::UsdsArbitrum
            | Self::UsdgArbitrum
            | Self::PyusdArbitrum
            | Self::XautArbitrum
            | Self::UsdcBase
            | Self::UsdsBase
            | Self::UsdtOptimism
            | Self::UsdcOptimism
            | Self::UsdsOptimism
            | Self::UsdtBep20
            | Self::UsdcBep20
            | Self::XautBep20
            | Self::UsdtPolygon
            | Self::UsdcPolygon
            | Self::PyusdPolygon
            | Self::XautPolygon
            | Self::UsdtAvalanche
            | Self::UsdcAvalanche
            | Self::UsdsAvalanche
            | Self::XautAvalanche
            | Self::UsdtSpl
            | Self::UsdcSpl
            | Self::UsdsSpl
            | Self::UsdgSpl
            | Self::PyusdSpl
            | Self::XautSpl
            | Self::PaxgSpl
            | Self::UsdtTrc20
            | Self::UsdtJetton
            | Self::XautJetton => true,
            Self::LinkErc20
            | Self::WbtcErc20
            | Self::WethErc20
            | Self::UniErc20
            | Self::ShibErc20
            | Self::EnaErc20
            | Self::AaveErc20
            | Self::OndoErc20
            | Self::PepeErc20
            | Self::ArbErc20
            | Self::LinkArbitrum
            | Self::WethArbitrum
            | Self::EnaArbitrum
            | Self::ArbArbitrum
            | Self::LinkBase
            | Self::WethBase
            | Self::EnaBase
            | Self::LinkOptimism
            | Self::WethOptimism
            | Self::EnaOptimism
            | Self::OpOptimism
            | Self::LinkBep20
            | Self::WethBep20
            | Self::EnaBep20
            | Self::LinkPolygon
            | Self::WethPolygon
            | Self::LinkAvalanche
            | Self::WethAvalanche
            | Self::LinkSpl
            | Self::WbtcSpl
            | Self::EnaSpl
            | Self::PumpSpl
            | Self::JupSpl
            | Self::WbtcTrc20
            | Self::EnaJetton => false,
        }
    }

    /// Binance-Peg USDT and USDC carry 18 decimals, unlike their Ethereum originals.
    pub const fn decimals(self) -> u8 {
        match self {
            Self::LinkErc20
            | Self::WethErc20
            | Self::UniErc20
            | Self::ShibErc20
            | Self::EnaErc20
            | Self::AaveErc20
            | Self::OndoErc20
            | Self::PepeErc20
            | Self::ArbErc20
            | Self::UsdsErc20
            | Self::DaiErc20
            | Self::PaxgErc20
            | Self::LinkArbitrum
            | Self::WethArbitrum
            | Self::EnaArbitrum
            | Self::ArbArbitrum
            | Self::UsdsArbitrum
            | Self::LinkBase
            | Self::WethBase
            | Self::EnaBase
            | Self::UsdsBase
            | Self::LinkOptimism
            | Self::WethOptimism
            | Self::EnaOptimism
            | Self::OpOptimism
            | Self::UsdsOptimism
            | Self::LinkBep20
            | Self::WethBep20
            | Self::EnaBep20
            | Self::UsdtBep20
            | Self::UsdcBep20
            | Self::LinkPolygon
            | Self::WethPolygon
            | Self::LinkAvalanche
            | Self::WethAvalanche
            | Self::UsdsAvalanche => 18,
            Self::LinkSpl | Self::EnaSpl => 9,
            Self::WbtcErc20 | Self::WbtcSpl | Self::WbtcTrc20 => 8,
            Self::UsdtErc20
            | Self::UsdcErc20
            | Self::UsdgErc20
            | Self::PyusdErc20
            | Self::XautErc20
            | Self::UsdtArbitrum
            | Self::UsdcArbitrum
            | Self::UsdgArbitrum
            | Self::PyusdArbitrum
            | Self::XautArbitrum
            | Self::UsdcBase
            | Self::UsdtOptimism
            | Self::UsdcOptimism
            | Self::XautBep20
            | Self::UsdtPolygon
            | Self::UsdcPolygon
            | Self::PyusdPolygon
            | Self::XautPolygon
            | Self::UsdtAvalanche
            | Self::UsdcAvalanche
            | Self::XautAvalanche
            | Self::PumpSpl
            | Self::JupSpl
            | Self::UsdtSpl
            | Self::UsdcSpl
            | Self::UsdsSpl
            | Self::UsdgSpl
            | Self::PyusdSpl
            | Self::XautSpl
            | Self::PaxgSpl
            | Self::UsdtTrc20
            | Self::EnaJetton
            | Self::UsdtJetton
            | Self::XautJetton => 6,
        }
    }

    /// Mainnet contract address, SPL mint, or jetton master.
    pub const fn contract(self) -> &'static str {
        match self {
            Self::LinkErc20 => "0x514910771AF9Ca656af840dff83E8264EcF986CA",
            Self::WbtcErc20 => "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
            Self::WethErc20 => "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            Self::UniErc20 => "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",
            Self::ShibErc20 => "0x95aD61b0a150d79219dCF64E1E6Cc01f0B64C4cE",
            Self::EnaErc20 => "0x57e114B691Db790C35207b2e685D4A43181e6061",
            Self::AaveErc20 => "0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9",
            Self::OndoErc20 => "0xfAbA6f8e4a5E8Ab82F62fe7C39859FA577269BE3",
            Self::PepeErc20 => "0x6982508145454Ce325dDbE47a25d4ec3d2311933",
            Self::ArbErc20 => "0xB50721BCf8d664c30412Cfbc6cf7a15145234ad1",
            Self::UsdtErc20 => "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            Self::UsdcErc20 => "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            Self::UsdsErc20 => "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
            Self::DaiErc20 => "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            Self::UsdgErc20 => "0xe343167631d89B6Ffc58B88d6b7fB0228795491D",
            Self::PyusdErc20 => "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",
            Self::XautErc20 => "0x68749665FF8D2d112Fa859AA293F07A622782F38",
            Self::PaxgErc20 => "0x45804880De22913dAFE09f4980848ECE6EcbAf78",
            Self::LinkArbitrum => "0xf97f4df75117a78c1A5a0DBb814Af92458539FB4",
            Self::WethArbitrum => "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            // Ethena deploys its LayerZero ENA at the same address on each of these networks.
            Self::EnaArbitrum | Self::EnaBase | Self::EnaOptimism | Self::EnaBep20 => {
                "0x58538e6A46E07434d7E7375Bc268D3cb839C0133"
            }
            Self::ArbArbitrum => "0x912CE59144191C1204E64559FE8253a0e49E6548",
            Self::UsdtArbitrum => "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
            Self::UsdcArbitrum => "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            Self::UsdsArbitrum => "0x6491c05A82219b8D1479057361ff1654749b876b",
            Self::UsdgArbitrum => "0x004B506865409877C9fA29bfb1ebA929984B9bbC",
            Self::PyusdArbitrum => "0x46850aD61C2B7d64d08c9C754F45254596696984",
            Self::XautArbitrum => "0x40461291347e1eCbb09499F3371D3f17f10d7159",
            Self::LinkBase => "0x88Fb150BDc53A65fe94Dea0c9BA0a6dAf8C6e196",
            // Every OP Stack network predeploys WETH at this address.
            Self::WethBase | Self::WethOptimism => "0x4200000000000000000000000000000000000006",
            Self::UsdcBase => "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            Self::UsdsBase => "0x820C137fa70C8691f0e44Dc420a5e53c168921Dc",
            Self::LinkOptimism => "0x350a791Bfc2C21F9Ed5d10980Dad2e2638ffa7f6",
            Self::OpOptimism => "0x4200000000000000000000000000000000000042",
            Self::UsdtOptimism => "0x01bFF41798a0BcF287b996046Ca68b395DbC1071",
            Self::UsdcOptimism => "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85",
            Self::UsdsOptimism => "0x4F13a96EC5C4Cf34e442b46Bbd98a0791F20edC3",
            Self::LinkBep20 => "0xF8A0BF9cF54Bb92F17374d9e9A321E6a111a51bD",
            Self::WethBep20 => "0x2170Ed0880ac9A755fd29B2688956BD959F933F8",
            Self::UsdtBep20 => "0x55d398326f99059fF775485246999027B3197955",
            Self::UsdcBep20 => "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
            Self::XautBep20 => "0x21cAef8A43163Eea865baeE23b9C2E327696A3bf",
            Self::LinkPolygon => "0x53E0bca35eC356BD5ddDFebbD1Fc0fD03FaBad39",
            Self::WethPolygon => "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619",
            Self::UsdtPolygon => "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
            Self::UsdcPolygon => "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359",
            Self::PyusdPolygon => "0x99aF3EeA856556646C98c8B9b2548Fe815240750",
            Self::XautPolygon => "0xF1815bd50389c46847f0Bda824eC8da914045D14",
            Self::LinkAvalanche => "0x5947BB275c521040051D82396192181b413227A3",
            Self::WethAvalanche => "0x49D5c2BdFfac6CE2BFdB6640F4F80f226bc10bAB",
            Self::UsdtAvalanche => "0x9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7",
            Self::UsdcAvalanche => "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E",
            Self::UsdsAvalanche => "0x86Ff09db814ac346a7C6FE2Cd648F27706D1D470",
            Self::XautAvalanche => "0x2775d5105276781B4b85bA6eA6a6653bEeD1dd32",
            Self::LinkSpl => "LinkhB3afbBKb2EQQu7s7umdZceV3wcvAUJhQAfQ23L",
            Self::WbtcSpl => "5XZw2LKTyrfvfiskJ78AMpackRjPcyCif1WhUsPDuVqQ",
            Self::EnaSpl => "72QvBVwpxqmheEPfaCwWSWqEFsUy3rhWt6JhQBMNTwD1",
            Self::PumpSpl => "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
            Self::JupSpl => "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",
            Self::UsdtSpl => "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
            Self::UsdcSpl => "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            Self::UsdsSpl => "USDSwr9ApdHk5bvJKMjzff41FfuX8bSxdKcR81vTwcA",
            Self::UsdgSpl => "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH",
            Self::PyusdSpl => "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo",
            Self::XautSpl => "AymATz4TCL9sWNEEV9Kvyz45CHVhDZ6kUgjTJPzLpU9P",
            Self::PaxgSpl => "5GgRAEmv8ZxF2PR5hY72Qs5x1bnQ6UK2RbTPoqJ3wSwW",
            Self::WbtcTrc20 => "TYhWwKpw43ENFWBTGpzLHn3882f2au7SMi",
            Self::UsdtTrc20 => "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
            Self::EnaJetton => "EQAFhZFZYye0llsl785OzOJG9QT6PXyYUdmiC4U95lBuHh1S",
            Self::UsdtJetton => "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs",
            Self::XautJetton => "EQA1R_LuQCLHlMgOo1S4G7Y7W1cd0FrAkbA10Zq7rddKxi9k",
        }
    }
}

/// A native coin or supported token. Decimals belong to the asset, while `Amount` stores
/// base units.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AssetId {
    Native(ChainId),
    Token(TokenId),
}

impl AssetId {
    #[cfg(feature = "test-util")]
    pub const COUNT: usize = ChainId::COUNT + TokenId::COUNT;

    /// Every native coin in [`ChainId::ALL`] order, then every token in [`TokenId::ALL`]
    /// order.
    pub fn all() -> impl Iterator<Item = Self> {
        ChainId::ALL.into_iter().map(Self::Native).chain(TokenId::ALL.into_iter().map(Self::Token))
    }

    pub const fn chain(self) -> ChainId {
        match self {
            Self::Native(chain) => chain,
            Self::Token(token) => token.chain(),
        }
    }

    pub const fn decimals(self) -> u8 {
        match self {
            Self::Native(chain) => chain.native_decimals(),
            Self::Token(token) => token.decimals(),
        }
    }

    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Native(chain) => chain.ticker(),
            Self::Token(token) => token.symbol(),
        }
    }

    pub const fn is_real_world_asset(self) -> bool {
        match self {
            Self::Native(_) => false,
            Self::Token(token) => token.is_real_world_asset(),
        }
    }

    /// The stable `<kind>:<name>` string the store's `asset` columns hold.
    pub fn key(self) -> String {
        match self {
            Self::Native(chain) => format!("native:{}", chain.as_str()),
            Self::Token(token) => format!("token:{}", token.as_str()),
        }
    }

    /// The inverse of [`AssetId::key`].
    pub fn parse_key(key: &str) -> Option<Self> {
        match key.split_once(':')? {
            ("native", chain) => ChainId::parse(chain).map(Self::Native),
            ("token", token) => TokenId::parse(token).map(Self::Token),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_asset_has_a_unique_stable_key_that_round_trips() {
        let mut seen = BTreeSet::new();
        for asset in AssetId::all() {
            let key = asset.key();
            assert!(seen.insert(key.clone()), "two assets spell themselves {key}");
            assert_eq!(AssetId::parse_key(&key), Some(asset), "{key}");
        }
        assert_eq!(AssetId::Native(ChainId::Bitcoin).key(), "native:bitcoin");
        assert_eq!(AssetId::Token(TokenId::UsdtErc20).key(), "token:usdt_erc20");
        assert_eq!(AssetId::parse_key("native:namecoin"), None);
        assert_eq!(AssetId::parse_key("usdt_erc20"), None);
        assert_eq!(AssetId::parse_key(""), None);
    }

    #[test]
    fn an_asset_takes_its_chain_scale_and_symbol_from_its_coin_or_token() {
        for asset in AssetId::all() {
            match asset {
                AssetId::Native(chain) => {
                    assert_eq!(asset.chain(), chain);
                    assert_eq!(asset.decimals(), chain.native_decimals());
                    assert_eq!(asset.symbol(), chain.ticker());
                }
                AssetId::Token(token) => {
                    assert_eq!(asset.chain(), token.chain());
                    assert_eq!(asset.decimals(), token.decimals());
                    assert_eq!(asset.symbol(), token.symbol());
                }
            }
        }
    }

    #[test]
    fn every_token_carries_its_network_specific_decimals() {
        assert_eq!(TokenId::UsdtBep20.decimals(), 18);
        assert_eq!(TokenId::UsdcBep20.decimals(), 18);
        assert_eq!(TokenId::LinkSpl.decimals(), 9, "Solana's LINK is scaled for its own mint");
        assert_eq!(TokenId::EnaSpl.decimals(), 9, "so is Solana's ENA");
        assert_eq!(TokenId::EnaJetton.decimals(), 6, "and TON's ENA has six");
        assert_eq!(TokenId::WbtcErc20.decimals(), 8, "wrapped bitcoin keeps bitcoin's satoshi");
        for token in TokenId::ALL {
            let expected = match token.symbol() {
                "LINK" | "ENA" if token.chain() == ChainId::Solana => 9,
                "ENA" if token.chain() == ChainId::Ton => 6,
                "WBTC" => 8,
                "LINK" | "WETH" | "UNI" | "SHIB" | "ENA" | "AAVE" | "ONDO" | "PEPE" | "ARB"
                | "OP" | "DAI" | "USDS" | "PAXG"
                    if token.chain() != ChainId::Solana =>
                {
                    18
                }
                "USDT" | "USDC" if token.chain() == ChainId::Bsc => 18,
                _ => 6,
            };
            assert_eq!(token.decimals(), expected, "{}", token.as_str());
        }
    }

    #[test]
    fn a_dollar_coin_keeps_its_name_and_symbol_on_every_chain() {
        for token in [TokenId::UsdgErc20, TokenId::UsdgArbitrum, TokenId::UsdgSpl] {
            assert_eq!(token.display_name(), "Global Dollar");
            assert_eq!(token.symbol(), "USDG");
        }
        for token in
            [TokenId::PyusdErc20, TokenId::PyusdPolygon, TokenId::PyusdArbitrum, TokenId::PyusdSpl]
        {
            assert_eq!(token.display_name(), "PayPal USD");
            assert_eq!(token.symbol(), "PYUSD");
        }
    }

    #[test]
    fn gold_tokens_keep_their_identity_and_network_specific_scale() {
        for (token, chain, symbol, name, decimals) in [
            (TokenId::PaxgErc20, ChainId::Ethereum, "PAXG", "PAX Gold", 18),
            (TokenId::PaxgSpl, ChainId::Solana, "PAXG", "PAX Gold", 6),
            (TokenId::XautErc20, ChainId::Ethereum, "XAUT", "Tether Gold", 6),
            (TokenId::XautArbitrum, ChainId::Arbitrum, "XAUT", "Tether Gold", 6),
            (TokenId::XautBep20, ChainId::Bsc, "XAUT", "Tether Gold", 6),
            (TokenId::XautPolygon, ChainId::Polygon, "XAUT", "Tether Gold", 6),
            (TokenId::XautAvalanche, ChainId::Avalanche, "XAUT", "Tether Gold", 6),
            (TokenId::XautSpl, ChainId::Solana, "XAUT", "Tether Gold", 6),
            (TokenId::XautJetton, ChainId::Ton, "XAUT", "Tether Gold", 6),
        ] {
            assert_eq!(token.chain(), chain);
            assert_eq!(token.symbol(), symbol);
            assert_eq!(token.display_name(), name);
            assert_eq!(token.decimals(), decimals);
        }
    }

    #[test]
    fn every_token_names_a_contract_on_its_own_chain() {
        for token in TokenId::ALL {
            let contract = token.contract();
            assert!(!contract.is_empty(), "{}", token.as_str());
            match token.chain() {
                ChainId::Ethereum
                | ChainId::Arbitrum
                | ChainId::Base
                | ChainId::Optimism
                | ChainId::Bsc
                | ChainId::Polygon
                | ChainId::Avalanche => {
                    assert!(contract.starts_with("0x"), "{contract}");
                    assert_eq!(contract.len(), 42, "{contract}");
                }
                ChainId::Tron => assert!(contract.starts_with('T'), "{contract}"),
                ChainId::Solana => assert!(!contract.starts_with("0x"), "{contract}"),
                ChainId::Ton => {
                    assert!(contract.starts_with("EQ"), "{contract}");
                    assert_eq!(contract.len(), 48, "{contract}");
                }
                other @ (ChainId::Bitcoin
                | ChainId::Litecoin
                | ChainId::Dogecoin
                | ChainId::BitcoinCash
                | ChainId::Zcash
                | ChainId::Xrp
                | ChainId::Monero) => {
                    panic!("{} has no tokens", other.as_str())
                }
            }
        }
    }

    #[test]
    fn only_dollar_and_gold_tokens_are_real_world_assets() {
        for asset in AssetId::all() {
            let is_dollar_or_gold = matches!(
                (asset, asset.symbol()),
                (
                    AssetId::Token(_),
                    "USDT" | "USDC" | "USDS" | "DAI" | "USDG" | "PYUSD" | "XAUT" | "PAXG"
                )
            );
            assert_eq!(asset.is_real_world_asset(), is_dollar_or_gold, "{}", asset.key());
        }
    }

    /// Tether's omnichain `USDT0` and `XAUt0` on Polygon read as the coins they
    /// stand for, so a holder sees one USDT and one XAUT across networks.
    #[test]
    fn polygon_tokens_read_as_the_coins_they_stand_for() {
        for (token, symbol, name) in [
            (TokenId::UsdtPolygon, "USDT", "Tether USD"),
            (TokenId::UsdcPolygon, "USDC", "USD Coin"),
            (TokenId::PyusdPolygon, "PYUSD", "PayPal USD"),
            (TokenId::XautPolygon, "XAUT", "Tether Gold"),
        ] {
            assert_eq!(token.chain(), ChainId::Polygon, "{}", token.as_str());
            assert_eq!(token.symbol(), symbol, "{}", token.as_str());
            assert_eq!(token.display_name(), name, "{}", token.as_str());
            assert_eq!(token.decimals(), 6, "{}", token.as_str());
        }
    }

    /// Every rollup pays gas in ether, so its coin trades as ETH.
    #[test]
    fn rollup_coins_are_ether() {
        for chain in [ChainId::Arbitrum, ChainId::Base, ChainId::Optimism] {
            let coin = AssetId::Native(chain);
            assert_eq!(coin.symbol(), "ETH", "{}", chain.as_str());
            assert_eq!(coin.decimals(), 18, "{}", chain.as_str());
        }
        for (token, chain, symbol, decimals) in [
            (TokenId::UsdtArbitrum, ChainId::Arbitrum, "USDT", 6),
            (TokenId::UsdcArbitrum, ChainId::Arbitrum, "USDC", 6),
            (TokenId::PyusdArbitrum, ChainId::Arbitrum, "PYUSD", 6),
            (TokenId::XautArbitrum, ChainId::Arbitrum, "XAUT", 6),
            (TokenId::UsdgArbitrum, ChainId::Arbitrum, "USDG", 6),
            (TokenId::UsdsArbitrum, ChainId::Arbitrum, "USDS", 18),
            (TokenId::UsdcBase, ChainId::Base, "USDC", 6),
            (TokenId::UsdsBase, ChainId::Base, "USDS", 18),
            (TokenId::OpOptimism, ChainId::Optimism, "OP", 18),
            (TokenId::UsdtOptimism, ChainId::Optimism, "USDT", 6),
            (TokenId::UsdcOptimism, ChainId::Optimism, "USDC", 6),
            (TokenId::UsdsOptimism, ChainId::Optimism, "USDS", 18),
        ] {
            assert_eq!(token.chain(), chain, "{}", token.as_str());
            assert_eq!(token.symbol(), symbol, "{}", token.as_str());
            assert_eq!(token.decimals(), decimals, "{}", token.as_str());
        }
    }

    /// AVAX lives on three chains, and the wallet holds it on the C-Chain.
    #[test]
    fn the_c_chain_coin_is_avax() {
        let coin = AssetId::Native(ChainId::Avalanche);
        assert_eq!(coin.symbol(), "AVAX");
        assert_eq!(ChainId::Avalanche.display_name(), "Avalanche C-Chain");
        assert_eq!(coin.decimals(), 18);
        for (token, symbol, name, decimals) in [
            (TokenId::UsdtAvalanche, "USDT", "Tether USD", 6),
            (TokenId::UsdcAvalanche, "USDC", "USD Coin", 6),
            (TokenId::XautAvalanche, "XAUT", "Tether Gold", 6),
            (TokenId::UsdsAvalanche, "USDS", "USDS", 18),
        ] {
            assert_eq!(token.chain(), ChainId::Avalanche, "{}", token.as_str());
            assert_eq!(token.symbol(), symbol, "{}", token.as_str());
            assert_eq!(token.display_name(), name, "{}", token.as_str());
            assert_eq!(token.decimals(), decimals, "{}", token.as_str());
        }
    }
}
