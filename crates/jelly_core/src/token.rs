use crate::chain::ChainId;
use crate::coin::Coin;
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

/// Ethena deploys its omnichain ENA at this address on Arbitrum, Base, Optimism, and BNB
/// Smart Chain.
const LAYERZERO_ENA: &str = "0x58538e6A46E07434d7E7375Bc268D3cb839C0133";

/// Every OP Stack network predeploys WETH at this address.
const OP_STACK_WETH: &str = "0x4200000000000000000000000000000000000006";

struct Deployment {
    /// What the token reads as, whatever its contract calls itself: Tether's omnichain
    /// `USDT0` and `XAUt0` and bridged copies such as Avalanche's `LINK.e` take the coin
    /// they stand for, and the ether Binance issues on BNB Smart Chain as `ETH` reads as
    /// WETH, like every other network's.
    coin: Coin,
    chain: ChainId,
    decimals: u8,
    contract: &'static str,
}

impl TokenId {
    pub const fn coin(self) -> Coin {
        self.deployment().coin
    }

    pub const fn chain(self) -> ChainId {
        self.deployment().chain
    }

    pub const fn decimals(self) -> u8 {
        self.deployment().decimals
    }

    /// Mainnet contract address, SPL mint, or jetton master.
    pub const fn contract(self) -> &'static str {
        self.deployment().contract
    }

    #[expect(clippy::too_many_lines, reason = "a data table with one row per token")]
    const fn deployment(self) -> Deployment {
        match self {
            Self::LinkErc20 => Deployment {
                coin: Coin::Link,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x514910771AF9Ca656af840dff83E8264EcF986CA",
            },
            Self::WbtcErc20 => Deployment {
                coin: Coin::Wbtc,
                chain: ChainId::Ethereum,
                decimals: 8,
                contract: "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
            },
            Self::WethErc20 => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            },
            Self::UniErc20 => Deployment {
                coin: Coin::Uni,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",
            },
            Self::ShibErc20 => Deployment {
                coin: Coin::Shib,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x95aD61b0a150d79219dCF64E1E6Cc01f0B64C4cE",
            },
            Self::EnaErc20 => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x57e114B691Db790C35207b2e685D4A43181e6061",
            },
            Self::AaveErc20 => Deployment {
                coin: Coin::Aave,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9",
            },
            Self::OndoErc20 => Deployment {
                coin: Coin::Ondo,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0xfAbA6f8e4a5E8Ab82F62fe7C39859FA577269BE3",
            },
            Self::PepeErc20 => Deployment {
                coin: Coin::Pepe,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x6982508145454Ce325dDbE47a25d4ec3d2311933",
            },
            Self::ArbErc20 => Deployment {
                coin: Coin::Arb,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0xB50721BCf8d664c30412Cfbc6cf7a15145234ad1",
            },
            Self::UsdtErc20 => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            },
            Self::UsdcErc20 => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            },
            Self::UsdsErc20 => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
            },
            Self::DaiErc20 => Deployment {
                coin: Coin::Dai,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            },
            Self::UsdgErc20 => Deployment {
                coin: Coin::Usdg,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0xe343167631d89B6Ffc58B88d6b7fB0228795491D",
            },
            Self::PyusdErc20 => Deployment {
                coin: Coin::Pyusd,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",
            },
            Self::XautErc20 => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0x68749665FF8D2d112Fa859AA293F07A622782F38",
            },
            Self::PaxgErc20 => Deployment {
                coin: Coin::Paxg,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x45804880De22913dAFE09f4980848ECE6EcbAf78",
            },
            Self::LinkArbitrum => Deployment {
                coin: Coin::Link,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: "0xf97f4df75117a78c1A5a0DBb814Af92458539FB4",
            },
            Self::WethArbitrum => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            },
            Self::EnaArbitrum => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: LAYERZERO_ENA,
            },
            Self::ArbArbitrum => Deployment {
                coin: Coin::Arb,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: "0x912CE59144191C1204E64559FE8253a0e49E6548",
            },
            Self::UsdtArbitrum => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
            },
            Self::UsdcArbitrum => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            },
            Self::UsdsArbitrum => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: "0x6491c05A82219b8D1479057361ff1654749b876b",
            },
            Self::UsdgArbitrum => Deployment {
                coin: Coin::Usdg,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0x004B506865409877C9fA29bfb1ebA929984B9bbC",
            },
            Self::PyusdArbitrum => Deployment {
                coin: Coin::Pyusd,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0x46850aD61C2B7d64d08c9C754F45254596696984",
            },
            Self::XautArbitrum => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0x40461291347e1eCbb09499F3371D3f17f10d7159",
            },
            Self::LinkBase => Deployment {
                coin: Coin::Link,
                chain: ChainId::Base,
                decimals: 18,
                contract: "0x88Fb150BDc53A65fe94Dea0c9BA0a6dAf8C6e196",
            },
            Self::WethBase => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Base,
                decimals: 18,
                contract: OP_STACK_WETH,
            },
            Self::EnaBase => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Base,
                decimals: 18,
                contract: LAYERZERO_ENA,
            },
            Self::UsdcBase => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Base,
                decimals: 6,
                contract: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            },
            Self::UsdsBase => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Base,
                decimals: 18,
                contract: "0x820C137fa70C8691f0e44Dc420a5e53c168921Dc",
            },
            Self::LinkOptimism => Deployment {
                coin: Coin::Link,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: "0x350a791Bfc2C21F9Ed5d10980Dad2e2638ffa7f6",
            },
            Self::WethOptimism => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: OP_STACK_WETH,
            },
            Self::EnaOptimism => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: LAYERZERO_ENA,
            },
            Self::OpOptimism => Deployment {
                coin: Coin::Op,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: "0x4200000000000000000000000000000000000042",
            },
            Self::UsdtOptimism => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Optimism,
                decimals: 6,
                contract: "0x01bFF41798a0BcF287b996046Ca68b395DbC1071",
            },
            Self::UsdcOptimism => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Optimism,
                decimals: 6,
                contract: "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85",
            },
            Self::UsdsOptimism => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: "0x4F13a96EC5C4Cf34e442b46Bbd98a0791F20edC3",
            },
            Self::LinkBep20 => Deployment {
                coin: Coin::Link,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: "0xF8A0BF9cF54Bb92F17374d9e9A321E6a111a51bD",
            },
            Self::WethBep20 => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: "0x2170Ed0880ac9A755fd29B2688956BD959F933F8",
            },
            Self::EnaBep20 => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: LAYERZERO_ENA,
            },
            // Binance-Peg USDT and USDC carry 18 decimals, unlike their Ethereum originals.
            Self::UsdtBep20 => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: "0x55d398326f99059fF775485246999027B3197955",
            },
            Self::UsdcBep20 => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
            },
            Self::XautBep20 => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Bsc,
                decimals: 6,
                contract: "0x21cAef8A43163Eea865baeE23b9C2E327696A3bf",
            },
            Self::LinkPolygon => Deployment {
                coin: Coin::Link,
                chain: ChainId::Polygon,
                decimals: 18,
                contract: "0x53E0bca35eC356BD5ddDFebbD1Fc0fD03FaBad39",
            },
            Self::WethPolygon => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Polygon,
                decimals: 18,
                contract: "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619",
            },
            Self::UsdtPolygon => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Polygon,
                decimals: 6,
                contract: "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
            },
            Self::UsdcPolygon => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Polygon,
                decimals: 6,
                contract: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359",
            },
            Self::PyusdPolygon => Deployment {
                coin: Coin::Pyusd,
                chain: ChainId::Polygon,
                decimals: 6,
                contract: "0x99aF3EeA856556646C98c8B9b2548Fe815240750",
            },
            Self::XautPolygon => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Polygon,
                decimals: 6,
                contract: "0xF1815bd50389c46847f0Bda824eC8da914045D14",
            },
            Self::LinkAvalanche => Deployment {
                coin: Coin::Link,
                chain: ChainId::Avalanche,
                decimals: 18,
                contract: "0x5947BB275c521040051D82396192181b413227A3",
            },
            Self::WethAvalanche => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Avalanche,
                decimals: 18,
                contract: "0x49D5c2BdFfac6CE2BFdB6640F4F80f226bc10bAB",
            },
            Self::UsdtAvalanche => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Avalanche,
                decimals: 6,
                contract: "0x9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7",
            },
            Self::UsdcAvalanche => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Avalanche,
                decimals: 6,
                contract: "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E",
            },
            Self::UsdsAvalanche => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Avalanche,
                decimals: 18,
                contract: "0x86Ff09db814ac346a7C6FE2Cd648F27706D1D470",
            },
            Self::XautAvalanche => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Avalanche,
                decimals: 6,
                contract: "0x2775d5105276781B4b85bA6eA6a6653bEeD1dd32",
            },
            Self::LinkSpl => Deployment {
                coin: Coin::Link,
                chain: ChainId::Solana,
                decimals: 9,
                contract: "LinkhB3afbBKb2EQQu7s7umdZceV3wcvAUJhQAfQ23L",
            },
            Self::WbtcSpl => Deployment {
                coin: Coin::Wbtc,
                chain: ChainId::Solana,
                decimals: 8,
                contract: "5XZw2LKTyrfvfiskJ78AMpackRjPcyCif1WhUsPDuVqQ",
            },
            Self::EnaSpl => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Solana,
                decimals: 9,
                contract: "72QvBVwpxqmheEPfaCwWSWqEFsUy3rhWt6JhQBMNTwD1",
            },
            Self::PumpSpl => Deployment {
                coin: Coin::Pump,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
            },
            Self::JupSpl => Deployment {
                coin: Coin::Jup,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",
            },
            Self::UsdtSpl => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
            },
            Self::UsdcSpl => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            },
            Self::UsdsSpl => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "USDSwr9ApdHk5bvJKMjzff41FfuX8bSxdKcR81vTwcA",
            },
            Self::UsdgSpl => Deployment {
                coin: Coin::Usdg,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH",
            },
            Self::PyusdSpl => Deployment {
                coin: Coin::Pyusd,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo",
            },
            Self::XautSpl => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "AymATz4TCL9sWNEEV9Kvyz45CHVhDZ6kUgjTJPzLpU9P",
            },
            Self::PaxgSpl => Deployment {
                coin: Coin::Paxg,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "5GgRAEmv8ZxF2PR5hY72Qs5x1bnQ6UK2RbTPoqJ3wSwW",
            },
            Self::WbtcTrc20 => Deployment {
                coin: Coin::Wbtc,
                chain: ChainId::Tron,
                decimals: 8,
                contract: "TYhWwKpw43ENFWBTGpzLHn3882f2au7SMi",
            },
            Self::UsdtTrc20 => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Tron,
                decimals: 6,
                contract: "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
            },
            Self::EnaJetton => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Ton,
                decimals: 6,
                contract: "EQAFhZFZYye0llsl785OzOJG9QT6PXyYUdmiC4U95lBuHh1S",
            },
            Self::UsdtJetton => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Ton,
                decimals: 6,
                contract: "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs",
            },
            Self::XautJetton => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Ton,
                decimals: 6,
                contract: "EQA1R_LuQCLHlMgOo1S4G7Y7W1cd0FrAkbA10Zq7rddKxi9k",
            },
        }
    }
}
