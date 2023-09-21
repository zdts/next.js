import React from 'react'
import Link from 'next/link'

export default function Page() {
  return (
    <div>
      <div id="page" style={{ padding: '10px' }}>
        Home
      </div>
      <div>
        <div style={{ padding: '10px' }}>
          <Link href="/no-suspense">
            /no-suspense - Without Suspense Boundary
          </Link>
        </div>
        <div style={{ padding: '10px' }}>
          <Link href="/suspense/node">/suspense/node - Node.js Runtime</Link>
        </div>
        <div style={{ padding: '10px' }}>
          <Link href="/suspense/edge">/suspense/edge - Edge Runtime</Link>
        </div>
      </div>
    </div>
  )
}
