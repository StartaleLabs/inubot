const fs = require('fs');

// Read the pool.json file
const poolData = JSON.parse(fs.readFileSync('./pool.json', 'utf8'));

// Function to convert hex to decimal
function hexToDecimal(hex) {
  if (!hex || typeof hex !== 'string' || !hex.startsWith('0x')) return hex;
  return parseInt(hex, 16).toString();
}

// Process all transactions
for (const address in poolData.pending) {
  const transactions = poolData.pending[address];
  
  for (const nonce in transactions) {
    const tx = transactions[nonce];
    
    // Convert fields from hex to decimal
    if (tx.type) tx.type = hexToDecimal(tx.type);
    if (tx.chainId) tx.chainId = hexToDecimal(tx.chainId);
    if (tx.nonce) tx.nonce = hexToDecimal(tx.nonce);
    if (tx.gasPrice) tx.gasPrice = hexToDecimal(tx.gasPrice);
    if (tx.gas) tx.gas = hexToDecimal(tx.gas);
    if (tx.value) tx.value = hexToDecimal(tx.value);

    delete tx.transactionIndex;
    delete tx.blockNumber;
    delete tx.blockHash;
    delete tx.r;
    delete tx.s;
    delete tx.v;
    delete tx.gas;
    delete tx.chainId;
  }
}

// Do the same for queued transactions if they exist
if (poolData.queued) {
  for (const address in poolData.queued) {
    const transactions = poolData.queued[address];
    
    for (const nonce in transactions) {
      const tx = transactions[nonce];
      
      if (tx.type) tx.type = hexToDecimal(tx.type);
      if (tx.chainId) tx.chainId = hexToDecimal(tx.chainId);
      if (tx.nonce) tx.nonce = hexToDecimal(tx.nonce);
      if (tx.gasPrice) tx.gasPrice = hexToDecimal(tx.gasPrice);
      if (tx.gas) tx.gas = hexToDecimal(tx.gas);
      if (tx.value) tx.value = hexToDecimal(tx.value);

      delete tx.transactionIndex;
      delete tx.blockNumber;
      delete tx.blockHash;
      delete tx.r;
      delete tx.s;
      delete tx.v;
      delete tx.gas;
      delete tx.chainId;
    }
  }
}

// Write the converted data back to a new file
fs.writeFileSync('./pool_decimal.json', JSON.stringify(poolData, null, 2));
console.log('Conversion complete. Output saved to pool_decimal.json');