// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./tokens.sol";

interface IOrganic {
    // deploy tokens
    function deploy_erc20() external returns (address);
    function deploy_erc721() external returns (address);
    function deploy_erc1155() external returns (address);

    // mint (burn/transfer) tokens, all of them (mint/burn/transfer) consume similar amount of gas
    function mint_erc20() external;
    function mint_erc721() external;
    function mint_erc1155() external;
}

contract Organic is IOrganic {
    address public erc20;
    address public erc721;
    address public erc1155;

    uint256 public erc721_next_id;
    uint256 public erc1155_next_id;

    constructor() {
        erc20 = address(new OpenERC20("TestERC20", "TEST-ERC20"));
        erc721 = address(new OpenERC721("TestERC721", "TEST-ERC721"));
        erc1155 = address(new OpenERC1155(""));
        erc721_next_id = 0;
        erc1155_next_id = 0;
    }

    function deploy_erc20() external override returns (address) {
        return address(new OpenERC20("TestERC20", "TEST-ERC20"));
    }

    function deploy_erc721() external override returns (address) {
        return address(new OpenERC721("TestERC721", "TEST-ERC721"));
    }

    function deploy_erc1155() external override returns (address) {
        return address(new OpenERC1155("DeployedERC1155"));
    }

    function mint_erc20() external override {
        OpenERC20(erc20).mint(msg.sender, 1);
    }

    function mint_erc721() external override {
        OpenERC721(erc721).mint(msg.sender, erc721_next_id++);
    }

    function mint_erc1155() external override {
        OpenERC1155(erc1155).mint(msg.sender, 1, 1);
    }
}
