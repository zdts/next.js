import React from 'react'

export default function Root({ children }) {
  return (
    <html>
      <body
        style={{
          display: 'flex',
          flexDirection: 'column',
          justifyContent: 'center',
          alignItems: 'center',
        }}
      >
        <div style={{ width: '400px' }}>{children}</div>
      </body>
    </html>
  )
}
